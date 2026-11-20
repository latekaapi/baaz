//! One open session: its model, and the centre pane that draws it.
//!
//! Two entities (docs/82 4.8, audit B S-9). A [`SessionRuntime`]
//! ([`runtime`]) is the session: one provider lane ([`lane`]) that folds
//! the provider's neutral `ProviderEvent`s into the session's `Session`
//! model, the turn state, the approvals and questions, the picks, the
//! outgoing attachments and the banners. It draws nothing and lives in the
//! application's session registry whether or not anything shows it, so a
//! turn runs to its end with no view. A [`SessionView`] — the entity the
//! spec calls `SessionView` (§2.3) — draws one runtime: the transcript list
//! and its caches, the composer draft, the scroll position, which cards the
//! person folded shut, the card fields. It sends the person's intents to
//! the runtime and maps each [`RuntimeEvent`] to one list operation.
//! Dropping a view loses only its drawing; closing the session drops the
//! runtime, and its transcript with it.
//!
//! Everything the view renders is a pure function of that model,
//! recomputed every frame. Nothing is optimistic except the word
//! "Working…", which the provider's first turn event immediately replaces
//! with the real turn.
//!
//! # The composer controls
//!
//! Every one is a neutral `provider::Command` sent through the seam, and the
//! chip is drawn from what came back, never from what was sent: model
//! (`SelectModel`), approval mode (`SelectApprovalMode`), compaction
//! (`CompactSession`), the queue strip (`SubmitInput` while busy, taken back
//! with `ReclaimQueued`) and steer (`SteerInput`). A control whose capability
//! the session's provider does not offer is not shown (docs/82 0.7).
//!
//! **Reasoning effort is the exception**: it rides on the submit and steer
//! commands and no provider echoes it back, so the effort chip shows the
//! client's own value.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use aui::composer::{
    command_menu, composer, composer_state_rows, effort_menu, effort_slider_menu, mention_picker, mode_menu,
    model_menu, plus_menu, provider_menu, queue_strip, PickerSubmenu, CommandItem, CommandSection, ComposerChip,
    ComposerChipAnchor, ComposerChipKind, ComposerChipState, ComposerIntent, MentionIcon, MentionItem, MentionSection,
    PickerRow, PlusMenuItem, QueueIntent, QueueStripRow,
};
use aui::data::{ContextMeterState, ContextPressure};
use aui::feedback::{banner, BannerActionStyle, BannerKind, BannerRun};
use aui::overlay::popover_layer;
#[cfg(feature = "probe")]
use aui::transcript::{MessageSelection, SelectionKey, TextSelection};
use aui::transcript::{
    AssistantTurnAction, LinkTarget, SpanEvent,
    ToolCardIntent, ToolGroupIntent, UserTurnAction, needs_you_banner, retry_row, status_row,
    StatusLead,
};
use aui_icons::IconName;
use aui_protocol::{ActivityState, Block, Delta, PermissionMode, PlanState, ReasoningEffort, Session, ThinkingState, ToolStatus, Turn};
use aui_tokens::scale;
use gpui::{
    div, list, prelude::*, px, AnyElement, ClipboardEntry, ClipboardItem, Context, Entity,
    EventEmitter, ExternalPaths, FocusHandle, Focusable, ListAlignment, ListState, Pixels, SharedString,
    Task, Window,
};
use gpui_kit::base::input::{InputEvent, Position, TextareaState};
use gpui_kit::component::input::Textarea;
use gpui_kit::base::{h_flex, v_flex};

use crate::overlays::{Command, Menu, MenuKind, Overlays};
use crate::transcript::{self, Cards, Folds, FullOutput, FullOutputState, PlanAction};
use crate::providers::{ExternalApprovalStore, ProviderId};
use crate::runtime::CaptureToken;
use crate::wire::WireCall;
use provider::Command as ProviderCommand;
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

/// A lane whose child dies twice within this long without finishing a turn
/// stops coming back on its own: the banner gives way to the typed failure
/// view (docs/71 §2), never a restart loop.
pub(crate) const LOST_TWICE_WINDOW: Duration = Duration::from_secs(30);
/// `view/page` takes 1–1000; the transport uses the ceiling and so does the
/// history backfill.
const PAGE_LIMIT: u32 = 1000;
/// The first backfill page's limit. The trace says page 1 of a 13 MB session
/// (790 events) fetches in ~3 ms and folds in ~11 ms, so a smaller first
/// page would buy nothing: every page, first included, runs at the ceiling.
const FIRST_PAGE_LIMIT: u32 = PAGE_LIMIT;
/// The transcript column's minimum side inset (docs/83 §2.5, §5): 32.
pub(crate) const TRANSCRIPT_PAD_X: f32 = scale::TRANSCRIPT_INSET;
/// The transcript column's maximum width (docs/83 §2.5, §5): 760, centred,
/// composer and the rows over it included. Centre-pane rows keep their
/// `TRANSCRIPT_PAD_X` gutters and centre their content inside them with
/// [`centred`], so below 760 px of content nothing changes.
pub(crate) const TRANSCRIPT_MEASURE: f32 = scale::TRANSCRIPT_MAX;
/// Bound a full-width centre-pane row to the transcript measure and centre it.
///
/// An inner `w_full` wrapper: capped at `TRANSCRIPT_MEASURE`, the leftover
/// split by auto margins.
fn centred(content: impl IntoElement) -> AnyElement {
    div().w_full().max_w(px(TRANSCRIPT_MEASURE)).mx_auto().child(content).into_any_element()
}

/// Whether one assistant turn's reply has fully arrived: at least one text
/// block, none of them still streaming, and no tool call still running.
///
/// What [`SessionRuntime::reply_complete_for_running_turn`] reads for the
/// running turn: a complete reply with the turn still open — once a token
/// reading says the model call that wrote it ended — means Muse is on the
/// `reminderChild` tail, and the turn reads finished.
fn reply_complete(turn: &Turn) -> bool {
    let Turn::Assistant { blocks, .. } = turn else { return false };
    let mut saw_text = false;
    for block in blocks {
        match block {
            Block::Text { streaming, .. } => {
                if *streaming {
                    return false;
                }
                saw_text = true;
            }
            Block::Thinking { state, .. } => {
                if *state == ThinkingState::Thinking {
                    return false;
                }
            }
            Block::Activity { state, .. } => {
                if *state == ActivityState::Working {
                    return false;
                }
            }
            Block::ToolCall { .. } => {
                if block.as_tool_call().is_some_and(|call| {
                    matches!(call.status, ToolStatus::Pending | ToolStatus::Running)
                }) {
                    return false;
                }
            }
            Block::ToolGroup { calls, state, summary: _ }
                if *state == ActivityState::Working
                    || calls
                        .iter()
                        .any(|call| matches!(call.status, ToolStatus::Pending | ToolStatus::Running)) =>
            {
                return false;
            }
            _ => {}
        }
    }
    saw_text
}
/// Top inset: the list starts under the centre header's line (docs/83 §3),
/// and the first turn sits this far below it — the 48 it sat below the v3
/// sheet's 32 px title row (canvas "Baaz v3": 80 from the sheet's top); §5
/// sets no other value, so task 2.6 kept it. One value for both sidebar
/// states.
const TRANSCRIPT_PAD_TOP: f32 = scale::SP_7 + scale::SP_5;
/// Between blocks of one turn, and between turns: the 16 / 26 reading
/// scale's rhythm (owner 2026-10-14; were 8 and 16).
const BLOCK_GAP: f32 = 14.0;
const TURN_GAP: f32 = 28.0;
/// The space under the composer's quiet row to the column's bottom edge:
/// 20 (owner 2026-10-14, Claude's shape: the box, its quiet row under it,
/// then air).
const COMPOSER_BOTTOM: f32 = 20.0;
/// The key context the transcript list wears, so ⌘C reaches the selection
/// copy without ever matching inside the composer or a card field.
pub const TRANSCRIPT_CONTEXT: &str = "BaazTranscript";
/// The ⌘C predicate for that copy: the transcript holds focus context, but
/// never the composer, a card field or the rename field.
pub const TRANSCRIPT_COPY_KEYS: &str =
    "BaazTranscript && !BaazComposer && !field && !BaazRename";
/// The height hint an unmeasured transcript **row** carries: a typical block
/// (a tool card, a short paragraph). The list is one item per block, not per
/// turn, so the hint is a block's, and the error on any one row is small
/// enough that a flick over unmeasured rows lands where it should.
pub(crate) const ROW_HEIGHT_HINT: f32 = 72.0;
/// The trailing spacer's row id in `row_counts`: one fixed-height row below
/// the final block. No turn carries this id, so the one
/// frame where the list runs ahead of its cache still renders it as the
/// spacer rather than as a turn.
pub(crate) const TAIL_SPACER_ID: &str = "tail-spacer";
/// The trailing spacer's height: room under the last block before the status
/// row or banner — 8 past the last turn's own 16, so the list ends 24 above
/// the composer (docs/41 §2.9), inside the seam's 32 px fade.
pub(crate) const TAIL_SPACER_H: f32 = scale::SP_3;
/// The gap the caret popovers leave above the composer, matching the
/// library's own `.pop{margin-bottom:8px}`.
const POPOVER_GAP: f32 = 8.0;
/// How tall a caret popover may grow before it scrolls. The `/` menu lists
/// every client command **and** every installed skill, and a workspace with
/// twenty skills would otherwise reach past the top of the window.
const POPOVER_MAX_H: f32 = 560.0;
/// What a caret menu's rows cannot have of the window's height: the centre
/// header with a margin (44 + 12), the composer band
/// (a two-line composer, 12 under it, status row: ~140), the popover gap and
/// the panel's padding and footer (8 + 12 + 30).
const CARET_MENU_CHROME: f32 = 56.0 + 140.0 + 50.0;
/// How many skill rows the `/` menu offers at once.
const SKILL_ROWS: usize = 8;

mod approvals;
mod checkpoints;
mod clocks;
mod commands;
mod composer;
mod events;
mod handoff;
mod lane;
mod runtime;
pub(crate) mod pickers;
mod questions;
pub(crate) mod render;
#[cfg(feature = "probe")]
mod scripting;
mod shell;
pub(crate) mod spans;
pub(crate) mod undo;

pub use runtime::{RuntimeEvent, SessionRuntime};
use crate::layout::LayoutGlobal as _;

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
    /// The window's terminal tabs, for the D51 live mirror and the
    /// "Open terminal" focus. `None` in replays and tests, where there is
    /// no host and cards render their folded text.
    pub terminal_host: Option<Entity<crate::terminal::TerminalHost>>,
}

/// Tests: one Muse notification frame for `session_id`, as the wire spells
/// it, for [`SessionRuntime::apply`].
#[cfg(test)]
pub(crate) fn wire_note(method: &str, session_id: &str, mut params: serde_json::Value) -> serde_json::Value {
    if let Some(map) = params.as_object_mut() {
        map.entry("sessionId").or_insert_with(|| session_id.into());
    }
    serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params})
}

/// One row of the model menu: what a [`provider::ModelSummary`] carries,
/// as the view reads it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ModelCatalogEntry {
    /// The model id a pick sends.
    pub model_id: String,
    /// The menu's label.
    pub display_label: String,
    /// The provider's one-line description.
    pub description: Option<String>,
    /// The session's effective model.
    pub is_active: bool,
    /// The provider's default row.
    pub is_default: bool,
    /// Which provider (or Muse model provider) serves the row.
    pub provider_id: String,
    /// The context window, when the provider reports one.
    pub context_limit: Option<u64>,
    /// The reasoning levels the provider lists for the row, in its order.
    pub efforts: Vec<String>,
    /// The level the model runs at when none is picked.
    pub default_reasoning_effort: Option<String>,
    /// The provider's words for each level it describes: `(level, words)`.
    pub effort_notes: Vec<(String, String)>,
}

impl ModelCatalogEntry {
    /// The row's selectable levels: the listed ones, else the described
    /// ones; `None` when the provider names none.
    pub fn effort_tiers(&self) -> Option<Vec<&str>> {
        if !self.efforts.is_empty() {
            return Some(self.efforts.iter().map(String::as_str).collect());
        }
        (!self.effort_notes.is_empty()).then(|| self.effort_notes.iter().map(|(tier, _)| tier.as_str()).collect())
    }

    /// The provider's words for `tier`, when it described it.
    pub fn effort_description(&self, tier: &str) -> Option<&str> {
        self.effort_notes.iter().find(|(name, _)| name == tier).map(|(_, words)| words.as_str())
    }
}

/// Where the quote bar hangs: the selection's two ends as pressed and
/// released (window coordinates) and the transcript's scroll offset then,
/// so the bar rides with the text.
#[derive(Clone, Copy, Debug)]
pub(crate) struct QuoteAnchor {
    pub start: gpui::Point<Pixels>,
    pub end: gpui::Point<Pixels>,
    pub scroll_y: Pixels,
}

/// The `@` picker's last completed rank and what it was ranked for. The
/// rank runs on the background executor (a 5 000-path subsequence scan per
/// keystroke does not belong on the UI thread); the render path reads this
/// cache and never ranks.
#[derive(Default)]
pub(crate) struct MentionRank {
    /// The ranked paths.
    cache: Vec<String>,
    /// The filter `cache` was ranked for.
    cache_for: String,
    /// The root `cache` was ranked over: ranks never cross roots.
    root: String,
    /// How many files the rank above ran over: a re-walk invalidates it.
    files_len: usize,
    /// The filter a background rank is computing, if any.
    pending: Option<String>,
    /// Monotonic id for mention ranks; only the latest result is kept.
    epoch: u64,
}

/// A shell block's output now: lines, running, exit, and a fetched full
/// output (lines, capped) when one landed.
pub type ToolOutputNow = (Vec<String>, bool, Option<i32>, Option<(Vec<String>, bool)>);

/// How long a sent quote's source span flashes, fading out: the Changes
/// rows' flash (right_changes `FLASH`).
pub(crate) const QUOTE_FLASH: std::time::Duration = std::time::Duration::from_millis(900);

/// One quotation in the composer (D4): the words and their source turn.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct QuoteDraft {
    /// The block's id.
    pub id: String,
    /// The quoted text, as Markdown for a cross-block prose quote.
    pub text: String,
    /// The turn it was quoted from (none for a hook's output).
    pub source: Option<String>,
    /// A code quote: its language (may be empty) and line count.
    pub code: Option<(String, usize)>,
    /// The selection it was quoted from and the span owner holding it (the
    /// turn, or a tool output's owner), so the sent quote can flash its
    /// source span when clicked (docs/43 §0). None for a hook's output.
    pub span: Option<(String, aui::transcript::MessageSelection)>,
}

/// An unsent composer's content: the text plus the images and files waiting
/// to go out with the next turn. One project's draft moved into another's
/// session travels as one of these.
#[derive(Clone, Default)]
pub(crate) struct Draft {
    /// The composer's text, verbatim.
    pub text: String,
    /// Images waiting to go out with the next turn.
    pub images: Vec<images::Image>,
    /// Files waiting to go out with the next turn, as extracted text.
    pub files: Vec<attachments::AttachedFile>,
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
    /// A turn failed because the credential is gone (a typed signal from
    /// the provider): the app raises its signed-out banner (D5 B5).
    SignedOut,
    /// The child exited; the application owns the reconnect.
    Closed,
    /// The lane's child died twice within [`LOST_TWICE_WINDOW`] without
    /// finishing a turn (docs/71 §2): the view grows its lost banner into
    /// the typed failure (its Retry revives the child); the application
    /// notes it in the log.
    ChildFailed {
        /// The last loss, in the adapter's words.
        message: String,
        /// The child's last stderr lines.
        stderr_excerpt: Vec<String>,
    },
    /// A lane's turn ended, and whether it failed (with the provider's
    /// words when it did): the row's "Failed" stands on this for every
    /// provider (docs/71 §6, closing the gap docs/70 §7 item 4 left).
    ProviderTurnOutcome {
        /// The lane session.
        session_id: String,
        /// The turn failed.
        failed: bool,
        /// The provider's message for a failure.
        error: Option<String>,
    },
    /// A window-level slash command typed in the composer or picked from its
    /// `/` menu. The session cannot act on the window, so it hands the command
    /// up to `Harness::run_window_command` — the same dispatch the ⌘K palette
    /// uses, so both routes run one implementation.
    WindowCommand(crate::overlays::Command),
    /// `/clear`: start a new session in this workspace.
    NewSession,
    /// A transcript skill row's tap: open the Skills page on that skill.
    /// The session cannot act on the window, so the application opens the
    /// page and selects the row (D63).
    OpenSkill {
        /// The skill name, as the quiet row's card target carried it.
        name: String,
    },
    /// B12: a turn fold's `+N −M` chip: open Changes on that turn's own
    /// diff (its checkpoints, docs/82 6.3), or on the working tree with its
    /// files open when it has none. Window-level, like the skill tap above.
    OpenTurnChanges {
        /// The folded turn's id.
        turn_id: String,
    },
    /// `ForkSession` on a provider lane succeeded: open the fork as a new
    /// provider-lane view on the same provider. The application connects
    /// a fresh child and resumes the fork there, so the source view's
    /// lane is never disturbed.
    ForkedOnProvider {
        /// The fork's session id, as the `ForkSession` ack minted it.
        session_id: String,
        /// The backend the fork runs on — the source view's own provider,
        /// never a switch.
        provider: ProviderId,
    },
    /// A provider lane admitted a turn: the `SubmitInput` ack named its
    /// turn. The application reveals the row, titles it from the prompt,
    /// and touches the local record — the lane's equivalent of the muse
    /// route's `turn/started` arm. The turn id itself stays in the view
    /// (which tracks the running turn); the application needs only the
    /// session and the prompt.
    ProviderTurnAccepted {
        /// The lane session that sent.
        session_id: String,
        /// What the person sent, verbatim — the title's source.
        prompt: String,
        /// The turn the ack admitted. An ack can arrive after its turn
        /// already finished, and the row must decline such a stale
        /// admission instead of manufacturing a fresh `Working · now`
        /// (W8c) — so the id travels with the event.
        turn_id: String,
    },
    /// A provider lane landed or settled an approval card: a
    /// `Block::Approval` arrived or left `Pending`. The application
    /// re-reads the open view's pending words into the sidebar row, so
    /// the row reads the needs-you state while the approval waits
    /// instead of `Working`.
    ProviderApprovalsChanged {
        /// The lane session whose cards moved.
        session_id: String,
    },
    /// The running turn's reply is whole and the provider is only doing
    /// its own bookkeeping now (Muse's memory-reminder tail): the turn
    /// reads finished. The application re-reads the row, so it stops
    /// reading `Working` while the turn stays open; titles and bylines
    /// still wait for the turn's own settle.
    ReplyComplete {
        /// The session whose reply completed.
        session_id: String,
    },
    /// A provider lane settled the turn it was running: a `TurnFinished`
    /// delta landed for it. The application records the byline, the
    /// ledger row and the record bump, and moves the one row — for every
    /// provider, Muse included (docs/70). Replayed history settles through
    /// [`SessionEvent::HistoryReplayed`] instead, once per batch; every
    /// write is idempotent, so a replay never double-counts the ledger.
    ProviderTurnFinished {
        /// The lane session that settled.
        session_id: String,
        /// The finished turn — half of the ledger key.
        turn_id: String,
        /// The turn's final counters, carrying the adapter-reported cost.
        meta: aui_protocol::TurnMeta,
    },
    /// A turn's checkpoint refs landed (docs/82 6.3): an open Changes
    /// screen on this session re-reads which scopes it can offer.
    CheckpointTaken {
        /// The session whose folder was snapshotted.
        session_id: String,
    },
    /// Undo (docs/82 6.4) asks first: put the confirm up. Its target is
    /// the session and `seq`, so only its own button runs the action.
    UndoConfirm {
        session_id: String,
        seq: u64,
        title: String,
        detail: String,
        /// The primary button: Undo or Revert.
        primary: &'static str,
    },
    /// An undo, a revert or the taking back of one landed, or was refused:
    /// the toast says so (with Undo when `undo` names the action), and
    /// Changes re-reads.
    UndoLanded {
        session_id: String,
        title: String,
        body: String,
        /// The action the toast's Undo takes back.
        undo: Option<u64>,
    },
    /// A fold batch that landed with nothing in flight finished turns: a
    /// reopen's replayed history (Claude Code and Codex replay a resumed
    /// session's stored turns as one batch). The application does its
    /// settle work once for all of them — one seed write, one batch of
    /// ledger rows, one row rejoin — where one `ProviderTurnFinished` per
    /// turn ran it N times (docs/81 A F14).
    HistoryReplayed {
        /// The lane session that replayed.
        session_id: String,
        /// Each replayed finish, in fold order: `(turn id, final counters)`.
        turns: Vec<(String, aui_protocol::TurnMeta)>,
    },
    /// The grown lost banner's typed action (Install / Sign in / Update,
    /// docs/71 §3): the application runs the D5 flow of that name for
    /// this session's provider.
    FailureAction(crate::provider_status::FailureAction),
    /// `/logout`.
    Logout,
    /// "Enable" on a disabled-provider banner: the application owns the
    /// Settings dialog.
    OpenProviders,
    /// "Run in a worktree" on the shared-folder warning (docs/82 6.2): the
    /// application moves this unsent draft into a worktree of its own.
    RunInWorktree,
    /// The new-session row's folder control (docs/82 6.6): the application
    /// lists the projects and opens the view's folder picker on them.
    FolderMenu,
    /// A folder-picker pick: the application moves this unsent draft into
    /// that project (or asks for a folder first).
    FolderPicked {
        /// The picked row.
        pick: pickers::FolderPick,
    },
    /// The new-session row's "Own worktree" toggle: the application moves
    /// this unsent draft into a worktree of its own, or back to the root.
    ToggleWorktree,
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
    /// The person picked a model in this session: the application stores it
    /// on the session's project defaults, so the next session there starts
    /// with it.
    ModelSelected {
        /// The picked model id.
        model_id: String,
    },
    /// The person picked a reasoning effort in this session: stored on the
    /// project defaults like the model.
    EffortSelected {
        /// The picked effort as the store spells it (`None` is "Default").
        effort: Option<String>,
    },
    /// The person picked an approval mode in this session: stored on the
    /// project defaults like the model.
    ModeSelected {
        /// The picked mode.
        mode: aui_protocol::PermissionMode,
    },
    /// The person picked another provider from the composer chip in a
    /// session that has never sent: nothing has been written and no lane
    /// has produced events, so the application retires this empty session
    /// and starts a new one on the pick. The pick also becomes the default
    /// for new sessions.
    SwitchProvider {
        /// The picked backend.
        provider: ProviderId,
    },
    /// The person picked "Hand off to X…" (composer provider menu, or ⌘K
    /// `/handoff`): the session's turns move to a fresh session on the
    /// pick as a lossy re-prompt (see [`crate::handoff`]). A same-provider
    /// pick never emits this — [`SessionView::pick_provider`] keeps it a
    /// menu close, and the run itself refuses it as a model change.
    HandoffRequested {
        /// The backend the fresh session starts on.
        provider: ProviderId,
        /// From the `handoff:<provider>` step verb: skip the confirm
        /// dialog, the person already asked on the command line.
        headless: bool,
    },
    /// "Open the new session" on a handoff card: show the destination.
    HandoffOpenSession {
        /// The fresh destination session.
        destination: String,
    },
    /// "Open the source session" on a fallback divider (no snapshot): show
    /// the retired source read-only, bypassing the chain-head redirect.
    HandoffOpenSource {
        /// The retired source session.
        source: String,
    },
    /// "Cancel" on a handoff card: abort the move while cancellable.
    HandoffCancel {
        /// The card's own block id.
        card_id: String,
    },
    /// The handoff pack's submit ack on the destination session: the run
    /// holding that destination advances to Acknowledged, epoch-fenced.
    HandoffPackAccepted {
        /// The destination session the pack landed on.
        session_id: String,
    },
    /// The pack submit failed on the destination: the run fails with the
    /// reason, and the source stays usable.
    HandoffPackFailed {
        /// The destination session the pack never landed on.
        session_id: String,
        /// What went wrong, in the provider's or the wire's words.
        reason: String,
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
    /// `/project`: open the Projects palette.
    Projects,
    /// `/fork` with nothing named: open the turn picker over the session's
    /// completed assistant turns.
    ForkPicker,
    /// The backfill's first page applied: the application pins the tail while
    /// later pages land. The view is already on screen; this swaps nothing.
    HistoryReady,
    /// The transcript came to rest at its end in a drawn frame — the
    /// person has read this session to the end, so the application clears
    /// its `DoneUnread` (task 3.1). Emitted on the frame the list arrives at
    /// its end, not on every frame it stays there.
    SeenToEnd,
    /// "Send anyway" on the pay-as-you-go banner: the person accepts the bill
    /// for the rest of this app run.
    TierOverride,
    /// "Check again" on the unknown-plan banner: re-probe the billing tier.
    TierRecheck,
    /// A transcript or terminal link to an `http(s)` URL (B9): the
    /// application opens the right pane on Browser and navigates there on
    /// the next render, instead of the system browser. `external` stays
    /// outside: a ⌘-click, or a `mailto:`/other scheme, rides
    /// `cx.open_url` like before.
    OpenUrl {
        /// The link destination, verbatim.
        url: String,
        /// When true the application opens it externally.
        external: bool,
    },
    /// A transcript link to a workspace path (B9): the application reveals
    /// it in the Files pane — a folder selected and scrolled into view, a
    /// file previewed with `highlight` banded — instead of the system
    /// opener. A ⌘-click keeps the old "open in default app" path and
    /// never emits this. No filesystem work happened on the way here;
    /// the application checks existence off the UI thread.
    RevealPath {
        /// The resolved absolute path.
        abs: PathBuf,
        /// 1-based lines to band, end exclusive (`42..43` is line 42).
        highlight: Option<std::ops::Range<u32>>,
    },
    /// D6: a shell card's full output, for the pane's output view.
    OpenOutput(crate::transcript::OutputOpen),
    /// D6: an image (a sent tile, a Markdown image) for the image view.
    OpenImage(PathBuf),
}


/// The billing guard's banner over the composer, as the
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
    /// A probe is in flight: the unknown-plan banner's "Check again" reads
    /// "Checking…" and pressing it again is a no-op.
    pub checking: bool,
}

/// A turn the server says is running.
struct Running {
    turn_id: String,
    started: Instant,
}

/// A user bubble folded at send, waiting on the provider's own user turn
/// to replace it (X1). The text rides along so a stop pressed before the
/// provider's first event can hand the prompt back to the composer (X1b).
struct PendingOptimistic {
    id: String,
    text: String,
}

/// How many completed turn ids a view remembers. A `turn/started` for a
/// remembered id is a re-delivered start from a re-attach, not new work —
/// and must not mark the view running (item 3). Turn ids accumulate one per
/// turn; past this many the set resets rather than growing without bound.
const MAX_COMPLETED_TURNS: usize = 1024;

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

/// A queued row whose reclaim (`turn/unqueue`) is in flight.
///
/// The text is captured when the row's button is clicked, from the same
/// `command_text` the fold would echo back — so handling `turn/unqueued`
/// never depends on that echo arriving on the same event tick, or at all.
/// A missing or late echo used to drop the reclaimed text silently.
#[derive(Clone, PartialEq, Eq)]
struct PendingUnqueue {
    kind: Unqueue,
    text: String,
}

/// The model picker's stand-in row when the catalog could not be answered:
/// its detail line carries the typed reason, and picking it re-states that
/// reason rather than going dead. Never a real model id, so it can never
/// collide with a catalog row.
const MODEL_UNAVAILABLE_ROW: &str = "__models_unavailable";

/// Where a view's own `session/start` stands (see [`SessionRuntime::begin_start`]).
#[derive(Clone, Debug, PartialEq, Eq)]
enum StartState {
    /// The server knows the session: every view but a fresh one.
    Started,
    /// The start is on the wire; what the person did meanwhile waits here.
    Starting(StartQueue),
    /// The start failed, so no session stands behind the view.
    Failed,
}

/// What the person did while the view's `session/start` was on the wire,
/// applied in order when it lands (`start_landed`): the latest pick per
/// setting, the catalog fetch an open picker asked for, then each send
/// exactly as it stood at its Enter. The server knows no session yet, so
/// none of these can leave earlier — and none may be dropped silently.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct StartQueue {
    model: Option<String>,
    mode: Option<PermissionMode>,
    /// A native plan toggle made while opening (docs/82 5.5), replayed
    /// as `SetPlanMode` once the lane is up.
    plan: Option<bool>,
    models: bool,
    sends: Vec<String>,
}

/// The centre pane over one session: the transcript list, the composer and
/// the card fields, drawn from its [`SessionRuntime`] (see the module doc).
pub struct SessionView {
    /// The session this view draws: its model, lane and turn state
    /// ([`SessionRuntime`]). The registry holds it too, so it outlives a
    /// dropped view.
    runtime: Entity<SessionRuntime>,
    /// The session's project display name for the empty state ("<provider>
    /// runs in …"): a rename changes it without touching the folder, so the
    /// folder name alone is not it. The window syncs it on every activation.
    project_name: Option<String>,
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
    /// Bumped on every write to [`Self::toggled`]: a row count memoised
    /// under an older generation is recomputed (see `row_memo`).
    toggled_gen: u64,
    /// Each turn's mapped row count from the last render-cache refresh,
    /// keyed by turn id, holding the snapshot `Rc` it was computed from (so
    /// its address cannot be reused by another turn while memoised), the
    /// settled flag, the toggle generation and the fold switch. A turn whose
    /// snapshot is the same `Rc` under the same inputs keeps its count: a
    /// streaming chunk recounts one turn, not the transcript (perf P1).
    row_memo: HashMap<String, render::RowMemo>,
    /// `(toggled_gen, fold_finished_turns)` at the last full refresh: a
    /// streaming refresh reuses the rows above its first changed turn only
    /// while these still hold.
    suffix_inputs: Option<(u64, bool)>,
    /// The first turn a render-cache refresh rebuilt since the last
    /// `sync_virtual_list`: the rows above it are unchanged there.
    rows_changed_from: Option<usize>,
    /// Virtualized transcript list (gpui `list()`, top-aligned, one item
    /// per turn) and the item count it was last synced to. Only visible rows
    /// are built and laid out per frame; `splice` keeps indices stable across
    /// folds (C1). Tail-follow rides `is_scrolled_to_end`/`scroll_to_end`,
    /// gated by the `follow` flag each fold change sets.
    list_state: ListState,
    list_len: usize,
    /// The list's rows: `(turn index, row within the turn)` for every row of
    /// every cached turn, rebuilt with the render cache. One item per
    /// **row** — a block, a bubble, a silent footer — never per turn: a
    /// visible list item is laid out whole every frame, and a real turn can
    /// run to hundreds of blocks.
    rows: Rc<Vec<(usize, usize)>>,
    /// Per cached turn, `(turn id, row count)`, so the next sync can find the
    /// first turn whose rows changed and splice from there, keeping the rows
    /// above it measured.
    row_counts: Vec<(String, usize)>,
    /// The list's last laid-out width. gpui forgets every row height, hints
    /// included, when the width changes; the frame after a change re-hints
    /// (`note_list_width`).
    list_width: Option<f32>,
    /// The list's viewport height last frame, and whether the list stood at
    /// its end when this frame began: a viewport that shrinks under a list at
    /// its end (the terminal dock opening, the window resizing) keeps it at
    /// the end instead of leaving "Jump to latest" up (owner 2026-10-15).
    list_height: Option<f32>,
    at_end_before: bool,
    /// Set when the width changed: the next sync re-hints the rows.
    rehint: bool,
    /// The `(turn id, row count)` list the virtual list was last synced to.
    synced_counts: Vec<(String, usize)>,
    /// B12: the busyness the row mapping was last built against: a settle
    /// flips the tail from live rows to the fold with no new turn, so the
    /// cache rebuilds on the flip.
    synced_busy: bool,
    /// Wheel events applied since the last traced paint, and when the last
    /// one landed (`BAAZ_FRAME_TRACE`, the hand-gesture instrument).
    /// Counted by the capture handler, drained by the next
    /// traced paint; both stay untouched unless the trace is on.
    trace_wheel_pending: usize,
    trace_last_wheel: Option<Instant>,
    /// A re-hint ran since the last traced paint (`BAAZ_FRAME_TRACE`,
    /// the resize instrument). Set beside the `rehint_rows`
    /// call, drained by the next traced paint; untouched unless the trace
    /// is on.
    trace_rehint: bool,
    /// Wheel travel accumulated since the last frame, in pixels (one
    /// offset per frame). The capture handler only adds to this;
    /// `render_transcript` drains it into exactly one `scroll_by` per frame,
    /// so per-frame work no longer grows with the event rate and the main
    /// queue is free for the display-link tick between events.
    pending_wheel: Pixels,
    /// How long the current wheel gesture stays open after its last event.
    /// The momentum tail arrives at 60 Hz, so 150 ms
    /// covers a missed sample; while it is in the future the transcript
    /// keeps presenting every tick, never re-hints, and never re-anchors
    /// the tail under the gesture.
    gesture_until: Option<Instant>,
    /// A re-hint owed but deferred past an in-flight gesture: a page landing or a width change while `gesture_until` is armed.
    /// The next frame after the gesture lapses pays it.
    rehint_deferred: bool,
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
    /// B8c: the Changes pane's "This session" edit list, cached on the view:
    /// [`session_edited_paths`](crate::transcript::session_edited_paths) walks
    /// every block, so the shell render only pays for it when
    /// [`Self::refresh_session_edits`] finds the
    /// [`edited_paths_cache_key`](crate::transcript::edited_paths_cache_key)
    /// moved. Transient view state, never persisted.
    cached_session_edits: Vec<String>,
    /// B8c: the key [`Self::cached_session_edits`] was built for, if ever.
    cached_session_edits_key: Option<(usize, usize)>,
    /// Set by every event that changed the transcript; the next frame consumes
    /// it and scrolls to the tail if the reader was already there.
    follow: bool,
    /// The floating composer band's height, reported by the window each
    /// frame it paints: the band sits over the list's bottom, so the list
    /// ends this far up (plus [`Self::stack_h`]) for its last line to
    /// scroll clear (owner 2026-10-15: the composer floats over the
    /// transcript).
    bottom_inset: f32,
    /// The bottom stack's measured height (status, banners, queue) — it
    /// floats over the list just above the band.
    stack_h: Rc<std::cell::Cell<f32>>,
    /// The transcript's cross-block spans (library span model), keyed by
    /// turn id: `(markdown source, span)`, plus one drag session per turn.
    /// One span at a time — a hover or pick that commits in one turn clears
    /// whatever another held — cleared on Escape or a plain click; ⌘C copies
    /// the held one (C8b). Per turn, not one shared cell: the library scopes
    /// cell keys (`p0`, `q0-b0`, …) to the markdown view that rendered them,
    /// so a single shared span would light up the same key in every turn at
    /// once. Keyed, not positional, so the span survives the transcript
    /// scrolling mid-drag.
    span_held: Rc<HashMap<String, (String, aui::transcript::MessageSelection)>>,
    span_sessions: HashMap<String, aui::transcript::SpanSession>,
    /// The quote flow (docs/43 §0): where the last press in the transcript
    /// landed, and the bar's anchor after a release that left a selection.
    quote_press: Option<gpui::Point<Pixels>>,
    quote_anchor: Option<QuoteAnchor>,
    /// The press began while the selection bar was up: a click (no drag)
    /// then dismisses the selection.
    quote_dismiss: bool,
    /// A sent quote's way back to its source in flight (docs/43 §0): the
    /// start, the from / to scroll offsets, and the turn it lands on.
    quote_scroll: Option<(std::time::Instant, gpui::Point<Pixels>, gpui::Point<Pixels>, gpui::ListOffset)>,
    /// The span to flash once the quote's scroll lands: (owner, span).
    quote_flash_pending: Option<(String, aui::transcript::MessageSelection)>,
    /// The source span flashing now (docs/43 §0 "flashes the source span's
    /// tint once"): start, span owner, span. Fades over [`QUOTE_FLASH`].
    quote_flash: Option<(std::time::Instant, String, aui::transcript::MessageSelection)>,
    /// The transcript box in window coordinates (the bar's frame).
    transcript_bounds: Option<gpui::Bounds<Pixels>>,
    /// The turn whose copy button shows the success check, if any: set on
    /// the copy intent and cleared after the library's `COPY_HOLD`, so the
    /// tick reads as a confirmation and then the copy glyph returns.
    copied_turn: Option<String>,
    /// Q1b: tool-call id → the wall time (epoch ms) its block first
    /// appeared. Stamped in `refresh_render_cache` where applied deltas
    /// materialise, read back through the transcript `Folds` so live run
    /// rows tick from the run's first member start.
    tool_first_seen: HashMap<String, u64>,
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
    /// The window's terminal tabs, for the D51 live mirror: `None` in
    /// replays and tests, where cards render their folded text.
    terminal_host: Option<Entity<crate::terminal::TerminalHost>>,
    /// [`Self::sent_items`] paired with the transcript's user turns, by turn
    /// id: refreshed with the render cache.
    sent_pairs: Rc<HashMap<String, crate::sent::SentItem>>,
    /// `pair_from`'s item cursor before each cached turn, and the
    /// `sent_items` length it was paired against: a streaming refresh
    /// re-pairs only the turns it rebuilt (speed S4).
    sent_cursors: Vec<usize>,
    sent_cursor_items: usize,
    /// The cached turns that hold card facts — a pending approval, an open
    /// question or a shell call — in turn order, so the per-refresh card
    /// state walks these, not every block of the transcript (speed S4:
    /// 0.79 ms a chunk at 3,000 turns).
    card_sites: Vec<render::CardSite>,
    /// `ts_prefix[i]`: the newest turn timestamp over cached turns `..i`
    /// (0 = none), so the deterministic clock a refresh reads is resumed
    /// at the rebuilt turn instead of re-walking the transcript.
    ts_prefix: Vec<u64>,
    /// A drag is over the window, so the drop overlay is up.
    dragging: bool,
    /// Whether the composer's draft trims to nothing.
    ///
    /// Maintained rather than read, because reading it meant copying the whole
    /// draft out of the textarea on every frame only to ask whether it was
    /// blank (finding `performance-8`). `InputEvent::Change` covers typing;
    /// [`Self::note_draft`] covers the writes that set the value
    /// programmatically, which emit no change event at all — the library's
    /// `set_value` suppresses them on purpose.
    draft_empty: bool,
    /// The `@` picker's background rank: what the render path reads, never
    /// ranks (see [`MentionRank`]).
    mention: MentionRank,
    focus: FocusHandle,
    tasks: Vec<Task<()>>,
    /// Held while a terminal card's command runs: the mirror's next refresh
    /// (see `TERMINAL_MIRROR_TICK`). One at a time; it clears itself.
    terminal_tick: Option<Task<()>>,
    /// The hidden pack bubble's turn id, found on the last cache refresh:
    /// what keeps the per-frame sync check O(1).
    handoff_pack_hidden: Option<String>,
    /// The pack acknowledgement's turn id — the assistant turn immediately
    /// following the hidden pack turn, hidden with it (the divider stands
    /// for both). Found on the same refresh; the turn stays in the provider
    /// history, only undrawn.
    handoff_pack_reply_hidden: Option<String>,
    _subscriptions: Vec<gpui::Subscription>,

}

impl EventEmitter<SessionEvent> for SessionView {}

impl WireCall for SessionView {
    fn wire_tasks(&mut self) -> &mut Vec<Task<()>> {
        &mut self.tasks
    }
}

impl SessionRuntime {
    /// A runtime over `session_id` with nothing live behind it: a replayed
    /// capture, an offline row, a test. It rides a read-only lane that
    /// answers the catalog and pending pulls with nothing and refuses every
    /// command that would act, saying why.
    pub(crate) fn read_only(session_id: String, provider_id: String, workspace: String, cx: &mut Context<Self>) -> Self {
        use provider::ProviderAdapter as _;
        let mut lane = provider_muse::replay::ReadOnly::new("Nothing is running behind this session here: it is read-only.");
        let _ = lane.connect(&crate::conn::connect_info());
        let (provider, events) = crate::conn::gate_quiet(provider::Provider::new(lane));
        let mut this = Self::new(session_id, provider_id, workspace);
        this.read_only = true;
        this.install_lane(provider, events, cx);
        this
    }
}

impl SessionView {
    /// A view over `session_id` with nothing live behind it: a replayed
    /// capture, an offline row, a test — over a fresh read-only runtime
    /// ([`SessionRuntime::read_only`]).
    pub fn new(session_id: String, host: SessionHost, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (provider_id, workspace) = (host.provider_id.clone(), host.workspace.clone());
        let runtime = cx.new(|cx| SessionRuntime::read_only(session_id, provider_id, workspace, cx));
        Self::over(runtime, host, window, cx)
    }

    /// A view over `runtime`: every view field, the composer and the card
    /// fields, and the two links to the runtime — each [`RuntimeEvent`]
    /// redraws this view (a list operation first, for the transcript's),
    /// and so does a notify (an `as_mut` write made from outside). Views
    /// are made on demand over a runtime the registry already holds, so a
    /// dropped (evicted) view is built again over the same session.
    pub fn over(runtime: Entity<SessionRuntime>, host: SessionHost, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let SessionHost { provider_id: _, workspace: _, overlays, capture, terminal_host } = host;
        let provider = runtime.read(cx).provider_kind();
        // The empty composer names the session's own provider: the person
        // is asking whoever this session runs on, never a hardcoded Muse.
        let composer_placeholder = provider.composer_placeholder();
        let composer =
            cx.new(|cx| composer_state_rows(composer_placeholder.clone(), 1, 8, window, cx));
        // Typing is what opens, filters and closes the caret popovers, and what
        // takes the composer off the history it was walking.
        let subscription = cx.subscribe(&composer, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.on_draft_changed(cx);
            }
        });
        // The feedback fields name the same provider: whoever reads the
        // answer is whoever the session runs on.
        let provider_label = provider.label();
        let feedback = cx.new(|cx| {
            composer_state_rows(format!("Why not? {provider_label} reads this."), 2, 4, window, cx)
        });
        let clarify = cx.new(|cx| {
            composer_state_rows(
                format!("Say what you would rather {provider_label} did"),
                2,
                4,
                window,
                cx,
            )
        });
        // B12 "Fold finished turns" (Settings → General) is read from the
        // layout global, never copied in: a flip re-folds this view, shown
        // or parked, when its rows were built under the other value.
        let layout_watch = cx.observe_global::<crate::layout::LayoutStore>(|this: &mut Self, cx| {
            let fold = crate::layout::Layout::get(cx).fold_finished_turns;
            if this.suffix_inputs.is_some_and(|(_, built)| built != fold) {
                this.refresh_render_cache(cx);
                cx.notify();
            }
        });
        // Whatever the runtime says moved (a turn, a banner, a chip, a clock)
        // this view draws; the root is not told. The runtime's own methods
        // name each change as a `RuntimeEvent`; a write made from outside
        // through `Entity::as_mut` notifies it instead (the borrow notifies
        // on drop), which this view also follows.
        let redraw = cx.observe(&runtime, |_, _, cx| cx.notify());
        let events = cx.subscribe(&runtime, |this: &mut Self, _, event: &RuntimeEvent, cx| {
            this.on_runtime_event(event, cx);
        });
        Self {
            runtime,
            project_name: None,
            composer,
            overlays,
            toggled: Rc::new(HashSet::new()),
            toggled_gen: 0,
            row_memo: HashMap::new(),
            suffix_inputs: None,
            rows_changed_from: None,
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
            rows: Rc::new(Vec::new()),
            row_counts: Vec::new(),
            list_width: None,
            rehint: false,
            synced_counts: Vec::new(),
            synced_busy: false,
            trace_wheel_pending: 0,
            trace_last_wheel: None,
            trace_rehint: false,
            pending_wheel: px(0.0),
            gesture_until: None,
            rehint_deferred: false,
            cached_turns: Rc::new(Vec::new()),
            cached_full_output: Rc::new(HashMap::new()),
            cached_pending_approval: None,
            cached_session_edits: Vec::new(),
            cached_session_edits_key: None,
            follow: true,
            bottom_inset: 0.0,
            list_height: None,
            at_end_before: true,
            stack_h: Rc::new(std::cell::Cell::new(0.0)),
            span_held: Rc::new(HashMap::new()),
            span_sessions: HashMap::new(),
            quote_press: None,
            quote_anchor: None,
            quote_dismiss: false,
            quote_scroll: None,
            quote_flash_pending: None,
            quote_flash: None,
            transcript_bounds: None,
            copied_turn: None,
            tool_first_seen: HashMap::new(),
            feedback_open: None,
            feedback,
            clarify_open: None,
            clarify,
            selections: HashMap::new(),
            previews: HashMap::new(),
            titles: Rc::new(HashMap::new()),
            at_rest: false,
            capture,
            terminal_host,
            sent_pairs: Rc::new(HashMap::new()),
            sent_cursors: Vec::new(),
            sent_cursor_items: 0,
            card_sites: Vec::new(),
            ts_prefix: Vec::new(),
            dragging: false,
            draft_empty: true,
            mention: MentionRank::default(),
            focus: cx.focus_handle(),
            tasks: Vec::new(),
            terminal_tick: None,
            handoff_pack_hidden: None,
            handoff_pack_reply_hidden: None,
            _subscriptions: vec![subscription, layout_watch, redraw, events],
        }
    }

    /// The runtime this view draws.
    pub fn runtime(&self) -> &Entity<SessionRuntime> {
        &self.runtime
    }

    /// The runtime's state, read for a frame or a question.
    pub fn rt<'a>(&self, cx: &'a gpui::App) -> &'a SessionRuntime {
        self.runtime.read(cx)
    }

    /// One runtime event, one list operation: transcript changes refresh the
    /// rows from the change and keep the tail; a decision closes the field
    /// open on it; stale menus re-read their checks; a toast goes up.
    fn on_runtime_event(&mut self, event: &RuntimeEvent, cx: &mut Context<Self>) {
        match event {
            RuntimeEvent::EntriesAppended
            | RuntimeEvent::EntryUpdated(_)
            | RuntimeEvent::EntriesRemoved
            | RuntimeEvent::ApprovalRequested => self.follow = true,
            RuntimeEvent::ApprovalDecided(approval_id) => self.close_feedback_for(approval_id),
            RuntimeEvent::CardsChanged => self.refresh_render_cache(cx),
            RuntimeEvent::MenusStale => {
                self.sync_model_menu_check(cx);
                self.sync_mode_menu_check(cx);
            }
            RuntimeEvent::TurnChanged
            | RuntimeEvent::Ticked
            | RuntimeEvent::BannerChanged
            | RuntimeEvent::ComposerChanged
            | RuntimeEvent::QuestionsChanged
            | RuntimeEvent::HistoryChanged
            | RuntimeEvent::HeaderChanged
            | RuntimeEvent::UndoChanged => {}
            RuntimeEvent::Toast { title, body } => {
                let (title, body) = (title.clone(), body.clone());
                self.overlays.update(cx, |overlays, cx| overlays.toast(title, body, cx));
            }
        }
        cx.notify();
    }
}

impl SessionRuntime {
    /// The folded transcript, once anything has arrived.
    pub fn session(&self) -> Option<&Session> {
        self.store.session()
    }

    /// Remember the text a request was sent with: retry resends it, the
    /// first prompt titles the row, the queue strip shows it.
    pub(crate) fn remember_command(&mut self, request_id: &str, text: &str) {
        self.store.record_command(request_id, text);
    }

    /// Append a client-authored block in a turn of its own (`id`).
    pub(crate) fn append_client_block(&mut self, id: &str, block: Block) -> Vec<Delta> {
        self.store.append_client_block(id, block)
    }

    /// Replace the first block of a client-authored turn.
    pub(crate) fn replace_client_block(&mut self, id: &str, block: Block) -> Vec<Delta> {
        self.store.replace_client_block(id, block)
    }

    /// Tests: fold one Muse wire frame ([`wire_note`]) through the same
    /// translation a hub lane runs, then into this view, as a live Muse
    /// session would see it.
    #[cfg(test)]
    pub(crate) fn apply(&mut self, frame: serde_json::Value, cx: &mut Context<Self>) {
        let events = self.test_wire.fold_line(&frame.to_string());
        self.fold_provider_events(events, cx);
    }

    #[cfg(feature = "probe")]
    /// Fold provider events as the lane task would: a replayed capture,
    /// folded at once on this update, and tests.
    pub(crate) fn fold_provider_events(&mut self, events: Vec<provider::ProviderEvent>, cx: &mut Context<Self>) {
        for event in events {
            self.on_provider_event(event, cx);
        }
    }
}

impl SessionView {
    /// B8c: the cached Changes-pane edit list when it is still fresh — `Some`
    /// when the turns have not moved under it — so the shell render pays no
    /// block scan on a steady frame. `None` means the caller must
    /// [`Self::refresh_session_edits`] under update.
    pub fn fresh_session_edits(&self, cx: &gpui::App) -> Option<Vec<String>> {
        let key = match self.runtime.read(cx).session() {
            Some(session) => crate::transcript::edited_paths_cache_key(&session.turns),
            None => crate::transcript::edited_paths_cache_key(&[]),
        };
        (self.cached_session_edits_key == Some(key)).then(|| self.cached_session_edits.clone())
    }
}

impl SessionRuntime {
    /// D6: a shell block's current output — lines, still running, exit —
    /// and the fetched full output when one landed, for the output view.
    pub fn tool_output(&self, block_id: &str) -> Option<ToolOutputNow> {
        let fetched = match self.full_outputs.get(block_id) {
            Some(crate::full_output::Fetch::Ready { lines, capped }) => Some((lines.clone(), *capped)),
            _ => None,
        };
        let session = self.session()?;
        for turn in &session.turns {
            let aui::protocol::Turn::Assistant { blocks, .. } = turn else { continue };
            for block in blocks {
                let calls: Vec<aui::protocol::ToolCall> = match block {
                    aui::protocol::Block::ToolGroup { calls, .. } => calls.clone(),
                    other => other.as_tool_call().into_iter().collect(),
                };
                for call in &calls {
                    if call.id == block_id {
                        if let aui::protocol::ToolBody::Shell { output_lines, exit_code, live, .. } = &call.body {
                            return Some((output_lines.clone(), *live, *exit_code, fetched));
                        }
                    }
                }
            }
        }
        None
    }

    /// D6: the files turn `turn_id` edited (its fold chip's files).
    pub fn turn_edits(&self, turn_id: &str) -> Vec<String> {
        self.session().map(|s| crate::transcript::turn_edited_paths(&s.turns, turn_id)).unwrap_or_default()
    }

    /// The agent's turns, by id, in transcript order: what the Changes
    /// screen numbers and orders a session's checkpoints by (docs/82 6.3).
    pub fn assistant_turn_ids(&self) -> Vec<String> {
        self.session()
            .map(|s| s.turns.iter().filter_map(|turn| match turn {
                aui::protocol::Turn::Assistant { id, .. } => Some(id.clone()),
                _ => None,
            }).collect())
            .unwrap_or_default()
    }
}

impl SessionView {
    /// B8c: recompute the Changes-pane edit list, store it with its key, and
    /// return it. Call only when [`Self::fresh_session_edits`] reports stale;
    /// with no session open this stores (and returns) the empty list.
    pub fn refresh_session_edits(&mut self, cx: &gpui::App) -> Vec<String> {
        let (key, edits) = match self.runtime.read(cx).session() {
            Some(session) => (
                crate::transcript::edited_paths_cache_key(&session.turns),
                crate::transcript::session_edited_paths(&session.turns),
            ),
            None => (crate::transcript::edited_paths_cache_key(&[]), Vec::new()),
        };
        self.cached_session_edits = edits.clone();
        self.cached_session_edits_key = Some(key);
        edits
    }
}

impl SessionRuntime {
    /// The last view cursor the fold observed, which is what a reconnect's
    /// `session/resume` needs.
    pub fn last_cursor(&self) -> Option<String> {
        Some(self.store.side().cursor.clone()).filter(|c| !c.is_empty())
    }

    /// Which registry entry this session was created on, read off the id
    /// the host fixed at construction. One field, one truth: there is no
    /// second copy to disagree with and no setter to change lanes live —
    /// a session is served by exactly one lane (see `crate::providers`).
    ///
    /// Every host names a registered provider — its key or a route it
    /// declares: boot refuses an unknown `--provider`, stored records are
    /// read strictly, and the app mints the id from a [`ProviderId`]
    /// everywhere else — so an id no provider answers to is a bug here,
    /// never a reason to run the session on some other provider.
    pub fn provider_kind(&self) -> ProviderId {
        ProviderId::resolve(&self.provider_id).unwrap_or_else(|error| panic!("a session host names no registered provider: {error}"))
    }

    /// Load the session's recorded bubble texts once, from its turns in
    /// `baaz.db`: what a resumed view folds replayed turns against after a
    /// restart, when the adapter's own map is gone with the old process.
    fn ensure_display_overrides(&mut self) {
        if self.display_overrides.is_none() {
            let (provider, session_id) = (self.provider_id.clone(), self.session_id.clone());
            let displays = crate::db::read(crate::db::path(), move |db| {
                crate::db::turns::displays(&db.conn, &provider, &session_id).unwrap_or_default()
            })
            .unwrap_or_default();
            self.display_overrides = Some(displays);
        }
    }

    /// Remember the bubble text for a provider submit carrying
    /// `display_text`: it waits for the provider's echo of the submit, which
    /// names the user turn it is then kept under (D-3, [`Self::display_override_for`]).
    /// The adapter folds already substitute live; the kept text is what a
    /// replayed history folds against after a restart.
    pub(crate) fn remember_display_text(&mut self, full_text: String, display: String) {
        if display.trim().is_empty() || display == full_text || full_text.len() > 131_072 {
            return;
        }
        self.ensure_display_overrides();
        if let Some(displays) = self.display_overrides.as_mut() {
            displays.by_text.insert(full_text.clone(), display.clone());
        }
        if self.pending_displays.len() < 64 {
            self.pending_displays.push((full_text, display));
        }
    }

    /// The bubble text for an echoed provider user turn: a submit waiting
    /// for this echo is kept under the turn now, in the database too; else
    /// what was kept for this turn, else (a turn recorded before turns were
    /// keyed, or a provider whose replay renames its turns) what was kept
    /// for its text.
    pub(crate) fn display_override_for(&mut self, turn_id: &str, text: &str) -> Option<String> {
        self.ensure_display_overrides();
        if let Some(at) = self.pending_displays.iter().position(|(full, display)| full == text || display == text) {
            let (full, display) = self.pending_displays.remove(at);
            if let Some(displays) = self.display_overrides.as_mut() {
                displays.by_turn.insert(turn_id.to_owned(), display.clone());
            }
            let (provider, session_id, turn_id, kept) =
                (self.provider_id.clone(), self.session_id.clone(), turn_id.to_owned(), display.clone());
            crate::db::run(crate::db::path(), move |db| {
                if let Err(error) = crate::db::turns::note_display(&db.conn, &provider, &session_id, &turn_id, &full, &kept) {
                    crate::baaz_log!("db: bubble text not kept ({error})");
                }
            });
            return Some(display);
        }
        let displays = self.display_overrides.as_ref()?;
        displays.by_turn.get(turn_id).or_else(|| displays.by_text.get(text)).cloned()
    }

    /// Whether this session has sent anything yet: folded turns, or a turn
    /// in flight (submitted with no `turn/started` yet, or the server says
    /// it runs). A turn in flight counts as turns — the lane is already
    /// committed — so the provider menu offers "New session on X" rather
    /// than a swap from the moment of send.
    pub fn has_turns(&self) -> bool {
        if self.is_sending() {
            return true;
        }
        self.session().is_some_and(|session| !session.turns.is_empty())
    }

    /// The steer-into-a-running-turn control's gate: `Some(reason)` means
    /// the control shows disabled with that reason, never present-and-broken.
    pub fn steer_gate(&self) -> Option<String> {
        crate::providers::gate(self.provider_kind(), provider::Capability::SteerTurn)
    }

    /// The stop/cancel control's gate, same contract as [`Self::steer_gate`].
    pub fn turn_gate(&self) -> Option<String> {
        crate::providers::gate(self.provider_kind(), provider::Capability::TurnControl)
    }

    /// The folder this session runs in, for tests outside this module.
    #[cfg(test)]
    pub(crate) fn workspace_for_tests(&self) -> &str {
        &self.workspace
    }

    /// Whether this session's provider offers `capability` (docs/82 0.7):
    /// what every control that sends a command asks first.
    pub(crate) fn offers(&self, capability: provider::Capability) -> bool {
        crate::providers::offers(self.provider_kind(), capability)
    }

    /// The question-answer control's gate, same contract as [`Self::steer_gate`].
    pub fn questions_gate(&self) -> Option<String> {
        crate::providers::gate(self.provider_kind(), provider::Capability::Questions)
    }

    /// Decisions the press sent that the server has not answered yet, for
    /// the provider lane to drain. The card waits on these; it never
    /// settles on the press.
    ///
    /// [`SessionRuntime::decide_external_approval`] drains this the moment a
    /// press lands, oldest first, through the lane's dispatch point.
    pub fn take_external_outbox(&mut self) -> Vec<ProviderCommand> {
        std::mem::take(&mut self.external_outbox)
    }

    /// The session's effective model id: the wire spelling, never display.
    /// The provider lane's pick wins while it stands: no server echo flows
    /// back into this fold for those sessions, so the id reads the
    /// recorded pick rather than going stale. On a provider lane the
    /// catalog's active row counts too — a fresh Codex open flags its
    /// effective model there, with no pick yet — then the session's own
    /// history (a reopen names the model it ran on), then the catalog's
    /// default row — then empty, so no `--model` flag is implied: the
    /// chip's display seed lives in [`Self::model`], never here. The muse
    /// lane reads the live fold and session first, then the same catalog
    /// the picker lists (active row, the session's own history, default
    /// row): a reopened session whose fold names no model still resolves
    /// its model instead of wearing the provider id. Only with nothing
    /// known at all does it fall back to the provider id, exactly as it
    /// always did.
    pub(crate) fn model_id(&self) -> String {
        if let Some(pending) = self.pending_model.as_deref() {
            return pending.to_owned();
        }
        if let Some(id) = self.store.side().model.as_ref().map(|m| m.id.clone()) {
            return id;
        }
        if let Some(id) = self.session().map(|s| s.model.clone()).filter(|m| !m.is_empty()) {
            return id;
        }
        if self.provider_kind().has(provider::ProviderTrait::CatalogFlagsSessionModel) {
            // A reopened muse session's fold names no model, but its
            // `model/list` catalog still flags the effective row — and the
            // session's own past still outranks the catalog's default.
            if let Some(id) =
                self.models.iter().find(|m| m.is_active).map(|m| m.model_id.clone())
            {
                return id;
            }
            if let Some(model) = self.history_model.clone() {
                return model;
            }
            if let Some(id) =
                self.models.iter().find(|m| m.is_default).map(|m| m.model_id.clone())
            {
                return id;
            }
            return self.provider_id.clone();
        }
        if let Some(id) = self.models.iter().find(|m| m.is_active).map(|m| m.model_id.clone()) {
            return id;
        }
        // The session's own past wins over any generic default: a
        // reopened session names the model its history ran on.
        if let Some(model) = self.history_model.clone() {
            return model;
        }
        // The catalog's default row (V1): a fresh session whose
        // catalog names no active row yet still chips a model.
        if let Some(id) = self.models.iter().find(|m| m.is_default).map(|m| m.model_id.clone())
        {
            return id;
        }
        String::new()
    }

    /// What the composer model chip reads. The muse lane shows the
    /// session model's display label from the `model/list` catalog when a
    /// row names it, else the model id humanised — never the bare provider
    /// id (`muse` is a lane, not a model). A provider lane shows the
    /// effective model's human name from the live catalog — never the
    /// provider's wire id (`claude-code` is not a model); with no model
    /// known yet, a Claude Code session chips the seed (env, `claude`
    /// settings, last reported model, then the out-of-box default),
    /// anything else the provider's human label.
    pub fn model(&self) -> SharedString {
        if self.provider_kind().has(provider::ProviderTrait::CatalogFlagsSessionModel) {
            return SharedString::from(self.muse_model_label());
        }
        let id = self.model_id();
        if id.is_empty() {
            // Display-only fallback: the id stays empty (nothing is
            // marked active, no pick is implied, no `--model` flag is
            // added) while the chip names the seed. The first init
            // frame still updates the chip exactly as before.
            if self.provider_kind() == crate::providers::ProviderId::ClaudeCode {
                return self
                    .claude_seed_label
                    .get_or_init(|| {
                        let workspace = if self.workspace.is_empty() {
                            None
                        } else {
                            Some(std::path::Path::new(self.workspace.as_str()))
                        };
                        let seed = crate::providers::claude_code_seed_model(workspace);
                        SharedString::from(crate::providers::claude_code_model_label(&seed))
                    })
                    .clone();
            }
            return SharedString::from(self.provider_kind().label().to_owned());
        }
        let label = self
            .models
            .iter()
            .find(|m| m.model_id == id)
            .map(|m| m.display_label.clone())
            .unwrap_or_else(|| {
                // No catalog row names this id (Claude Code reports full
                // ids the supplied alias menu never lists): humanise it
                // for the chip rather than wearing the raw wire id. Other
                // lanes keep today's raw fallback.
                if self.provider_kind() == crate::providers::ProviderId::ClaudeCode {
                    crate::providers::claude_code_model_label(&id)
                } else {
                    id.clone()
                }
            });
        SharedString::from(label)
    }

    /// This view's cached muse `model/list` ids, for the muse start path:
    /// what [`crate::projects::muse_start_model`] checks a stored default
    /// against. A provider lane's catalog is another lane's list and reads
    /// as nothing here, and a muse view with no catalog yet contributes
    /// nothing — an empty cache never drops a default.
    pub(crate) fn cached_muse_catalog_ids(&self) -> Vec<String> {
        if !self.provider_kind().has(provider::ProviderTrait::CatalogFlagsSessionModel) {
            return Vec::new();
        }
        self.models.iter().map(|m| m.model_id.clone()).collect()
    }

    /// Seed this view's cached muse `model/list` catalog in tests: one row
    /// per id, so the muse start path's catalog check has rows to read
    /// without a wire child behind the view.
    #[cfg(test)]
    pub(crate) fn seed_test_catalog(&mut self, ids: &[&str]) {
        self.models = ids
            .iter()
            .map(|id| ModelCatalogEntry {
                context_limit: None,
                description: None,
                display_label: (*id).to_owned(),
                is_active: false,
                is_default: false,
                model_id: (*id).to_owned(),
                provider_id: "muse".to_owned(),
                ..Default::default()
            })
            .collect();
    }

    /// What the muse lane's composer chip reads: the catalog's display
    /// label when a row names the session's model, else the model id
    /// humanised for display — never the bare provider id.
    fn muse_model_label(&self) -> String {
        let id = self.model_id();
        // Nothing names a model yet while Muse (or this draft's session on
        // it) is still starting: say so, never the bare lane name "Muse".
        if self.muse_starting || (matches!(self.start, StartState::Starting(_)) && id == self.provider_id) {
            return format!("Starting {}\u{2026}", self.provider_kind().label());
        }
        if let Some(label) =
            self.models.iter().find(|m| m.model_id == id).map(|m| m.display_label.clone())
        {
            return label;
        }
        Self::humanise_muse_model_id(&id)
    }

    /// A catalog row's label fit to show: muse 1.4.10's `model/list` sends
    /// the wire id as `displayLabel` (`muse-wonder-full-1.0`), so a label
    /// that is just the id reads humanised (`Muse Wonder Full 1.0`) — the
    /// chip, the menu rows and the active tag then all name a model the
    /// same way. A real display label passes through.
    pub(crate) fn muse_row_label(model_id: &str, label: &str) -> String {
        if label.is_empty() || label == model_id {
            Self::humanise_muse_model_id(model_id)
        } else {
            label.to_owned()
        }
    }

    /// Say whether Muse is still starting under this (live, local) draft.
    pub(crate) fn set_muse_starting(&mut self, starting: bool, cx: &mut Context<Self>) {
        if self.muse_starting != starting {
            self.muse_starting = starting;
            cx.emit(RuntimeEvent::MenusStale);
        }
    }

    /// A muse model id made fit for the chip: `muse-everyday` reads
    /// `Muse Everyday`. Words split on `-`/`_`, each capitalised; anything
    /// else passes through unchanged. The provider id itself humanises to
    /// its human label (`muse` → `Muse`), so a session with no model known
    /// yet still never wears the wire id.
    fn humanise_muse_model_id(id: &str) -> String {
        let words: Vec<String> = id
            .split(['-', '_'])
            .filter(|word| !word.is_empty())
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    None => String::new(),
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                }
            })
            .collect();
        if words.is_empty() { id.to_owned() } else { words.join(" ") }
    }

    /// The context meter's state: the provider's own reading of the
    /// context the next request carries, joined with the session's
    /// cumulative counters for the breakdown. `None` — the meter absent —
    /// until a provider that reports context has reported it (docs/82 5.5:
    /// the meter shows real context use or nothing; the old summed "N
    /// tokens" is gone).
    ///
    /// A provider whose context moves live ([`provider::ProviderTrait::
    /// LiveContextMeter`], Muse) always has a meter: before its first
    /// `session/contextUsage` it reads the cumulative totals, and a meter
    /// that appeared only after the first turn would move the toolbar
    /// under the person's hand. Claude Code and Codex report a reading at
    /// each turn's end (each request's on Codex).
    pub fn context(&self) -> Option<ContextMeterState> {
        let side = self.store.side();
        let reading = self.fake_context.clone().or_else(|| side.context.clone());
        if self.provider_kind().has(provider::ProviderTrait::LiveContextMeter) {
            let cumulative = side.totals.clone();
            let (used, window, pressure) = match reading {
                Some(usage) => (usage.used_tokens, usage.window_tokens, pressure(usage.pressure)),
                None => (cumulative.total_tokens, None, ContextPressure::Normal),
            };
            return Some(ContextMeterState {
                used_tokens: used,
                window_tokens: window,
                pressure,
                prompt_tokens: cumulative.prompt_tokens,
                output_tokens: cumulative.output_tokens,
                total_tokens: cumulative.total_tokens,
            });
        }
        let reading = reading?;
        let (prompt_tokens, output_tokens, _) = self.turn_totals();
        Some(ContextMeterState {
            used_tokens: reading.used_tokens,
            window_tokens: reading.window_tokens,
            pressure: pressure(reading.pressure),
            prompt_tokens,
            output_tokens,
            total_tokens: prompt_tokens + output_tokens,
        })
    }

    /// Every finished assistant turn's `TurnMeta` summed: prompt tokens,
    /// output tokens and cost (USD, zero where the provider reports none).
    /// Cache tokens are deliberately excluded: `TurnMeta` warns they may
    /// sit inside `tokens_in` depending on the provider's convention, so
    /// adding them double-counts.
    pub(crate) fn turn_totals(&self) -> (u64, u64, f64) {
        let mut prompt_tokens = 0u64;
        let mut output_tokens = 0u64;
        let mut cost = 0.0f64;
        if let Some(session) = self.session() {
            for turn in session.turns.iter() {
                if let Turn::Assistant { meta, .. } = turn {
                    prompt_tokens += meta.tokens_in;
                    output_tokens += meta.tokens_out;
                    cost += meta.cost_usd;
                }
            }
        }
        (prompt_tokens, output_tokens, cost)
    }

    /// The approval mode chip's label: the provider's own word for the
    /// mode in force (docs/82 5.5), the protocol's for one it does not
    /// declare.
    pub fn mode_label(&self) -> &'static str {
        let mode = self.mode();
        self.provider_kind().descriptor().mode(mode).map_or_else(|| mode.label(), |declared| declared.label)
    }

    /// The approval modes this session's provider runs, in its own picker
    /// order: the mode picker lists exactly these (docs/82 5.5).
    pub(crate) fn modes(&self) -> Vec<PermissionMode> {
        self.provider_kind().descriptor().modes.iter().map(|declared| declared.id).collect()
    }

    /// The approval mode the server says the session is in — or, while the
    /// start is on the wire, the pick waiting to be applied when it lands,
    /// else the mode an opening provider view asked its child for.
    fn mode(&self) -> PermissionMode {
        if let StartState::Starting(StartQueue { mode: Some(mode), .. }) = &self.start {
            return *mode;
        }
        if let Some(mode) = self.opening_mode() {
            return mode;
        }
        self.session().map(|s| s.mode).unwrap_or_default()
    }

    /// Whether a turn is running, which is what the send button morphs on.
    /// Forget the running turn: its id, its ticker, and the last elapsed
    /// tick the footer painted.
    ///
    /// The three move together. A ticker that outlives its turn is exactly
    /// what paints a finished turn as still counting, and there is no way
    /// out of that from the window.
    pub(crate) fn clear_running(&mut self) {
        self.running = None;
        self.reply_done = None;
        self.ticker = None;
        self.last_tick_secs = None;
    }

    #[cfg(feature = "probe")]
    /// Load a canned transcript into a read-only view (the
    /// `browser-chat-fixture` step: the agent card and line with no turn).
    pub(crate) fn load_fixture(&mut self, session: aui_protocol::Session, cx: &mut Context<Self>) {
        self.store.load(session);
        cx.emit(RuntimeEvent::EntriesAppended);
    }

    /// The transcript this view renders, when its session has loaded: what
    /// the agent card maps onto the library's card (R3).
    pub(crate) fn transcript_session(&self) -> Option<&aui_protocol::Session> {
        self.store.session()
    }

    pub fn busy(&self) -> bool {
        self.running.is_some() || self.submitting
    }

    /// Whether the provider lane already saw this turn finish: a late
    /// admission or a re-delivered start for it is stale, never new work.
    /// The application reads this off the [`SessionEvent::ProviderTurnAccepted`]
    /// turn id before arming the row (W8c).
    pub fn provider_turn_finished(&self, turn_id: &str) -> bool {
        self.completed_turns.contains(self.streamed_turn_id(turn_id))
    }

    /// The id the turn an ack named streams under: the adapter's
    /// `TurnIdentified` alias when one arrived, else the ack's own id
    /// (Codex and Muse ack the streamed id itself).
    pub(crate) fn streamed_turn_id<'a>(&'a self, accepted: &'a str) -> &'a str {
        self.turn_aliases.get(accepted).map_or(accepted, String::as_str)
    }

    /// What the sidebar row stands on while this session waits on a person:
    /// the newest pending approval's exact command and the newest
    /// unanswered question's prompt, in fold order (forward walk, last hit
    /// wins — the same block the transcript's own phase scan reads first).
    /// Live off the fold, so a caller that just applied an event reads the
    /// event's world, never a stale snapshot. Provider-lane taps live
    /// beside the fold, not inside it, so the lane's newest pending
    /// headline stands in when the fold holds nothing — without this a
    /// Codex approval mid-turn read `Working` on the row.
    /// Whether `turn_id` names a turn that already ended: finished on the
    /// lane, or re-delivered after finishing. What [`Self::row_pending`]
    /// reads so a turn that is over settles its cards for row purposes.
    fn turn_over(&self, turn_id: &str) -> bool {
        self.completed_turns.contains(turn_id)
    }

    pub fn row_pending(&self) -> (Option<String>, Option<String>) {
        use aui_protocol::ApprovalState;
        let mut approval: Option<String> = None;
        let mut question: Option<String> = None;
        if let Some(session) = self.store.session() {
            for turn in session.turns.iter() {
                // A turn that is over settles its cards for row purposes:
                // the fold moves a card off Pending only on a server echo,
                // and a cancelled turn (Codex "Deny and stop") can end with
                // no echo — its card would read Pending forever, pinning
                // the row on "Needs approval" (B3b).
                if self.turn_over(turn.id()) {
                    continue;
                }
                for block in turn.blocks() {
                    match block {
                        Block::Approval { command, state: ApprovalState::Pending, .. } => {
                            approval = Some(command.clone());
                        }
                        Block::Question { prompt, answer: None, .. } => {
                            question = Some(prompt.clone());
                        }
                        _ => {}
                    }
                }
            }
        }
        if approval.is_none() {
            approval = self.external_approvals.pending().last().map(|tap| tap.headline.clone());
        }
        if question.is_none() {
            question = self.external_questions.newest_headline();
        }
        (approval, question)
    }

    /// Whether the running turn's reply has fully arrived while the turn is
    /// still open: after the `agentMessage` completes and `session/tokenUsage`
    /// arrives, Muse runs `reminderChild` items (memory reminders) for 30–70 s
    /// before `turn/completed`.
    #[cfg(test)]
    pub fn reply_complete_for_running_turn(&self) -> bool {
        self.reply_complete_in_fold()
    }

    /// [`Self::reply_complete_for_running_turn`] off the fold itself, which
    /// a lane event has just moved and the render cache has not seen yet.
    fn reply_complete_in_fold(&self) -> bool {
        let Some(running) = self.running.as_ref() else { return false };
        self.store
            .session()
            .and_then(|session| session.turns.iter().rev().find(|turn| turn.id() == running.turn_id))
            .is_some_and(reply_complete)
    }

    /// A lane event moved the fold or the facts: re-read whether the
    /// running turn's reply is done. `usage_landed` is a fresh token-usage
    /// reading — a model call just ended — which, over a whole reply, says
    /// the call ended on the reply rather than on a tool call still being
    /// composed. Only a provider with a reminder tail tracks it. Emits
    /// [`SessionEvent::ReplyComplete`] on the edge, so the row re-reads.
    pub(super) fn note_reply_progress(&mut self, usage_landed: bool, cx: &mut Context<Self>) {
        if !self.provider_kind().has(provider::ProviderTrait::ReminderTail) {
            return;
        }
        let complete = self.reply_complete_in_fold();
        if !complete {
            self.reply_done = None;
            return;
        }
        if usage_landed && self.reply_done.is_none() {
            self.reply_done = self.running.as_ref().map(|running| running.turn_id.clone());
            cx.emit(SessionEvent::ReplyComplete { session_id: self.session_id.clone() });
            cx.emit(RuntimeEvent::TurnChanged);
        }
    }

    /// Whether the running turn is on Muse's `reminderChild` tail: its reply
    /// is whole, the model call that wrote it has ended, and the turn stays
    /// open. Only Muse reports that tail — a Codex or Claude Code turn open
    /// past its reply is still working on something the provider never
    /// named (seen on a Codex turn, 2026-10-17).
    pub fn on_reminder_tail(&self) -> bool {
        self.provider_kind().has(provider::ProviderTrait::ReminderTail)
            && self.running.as_ref().is_some_and(|running| self.reply_done.as_deref() == Some(running.turn_id.as_str()))
            && self.reply_complete_in_fold()
    }

    /// The reminder tail reads as a finished turn (owner 2026-10-30): the
    /// reply is done and nothing waits on the person. The composer is
    /// ready, the row stops reading `Working`, the transcript draws the
    /// turn settled, and a quiet note says the provider is saving
    /// memories. The turn itself stays open until its `turn/completed`.
    pub fn saving_memories(&self) -> bool {
        self.on_reminder_tail()
            && !self.waiting_on_you().is_some_and(|(approvals, questions)| approvals + questions > 0)
    }

    /// Whether the session reads as working: busy, and not on a tail that
    /// reads as finished. What the stop button, the inline progress row,
    /// the live transcript tail and the sidebar row stand on; the stop
    /// itself (Escape) and the guards that must not race the provider
    /// still read [`Self::busy`].
    pub fn working(&self) -> bool {
        self.busy() && !self.saving_memories()
    }
}

impl SessionView {
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
    /// person mid-thought must never have a keystroke mean "allow". A parked
    /// provider tap owns them the same way a muse card does (V1): either
    /// surface pending is enough.
    pub fn card_has_keys(&self, cx: &gpui::App) -> bool {
        self.draft_is_empty(cx)
            && !self.card_field_open()
            && (self.newest_pending_approval().is_some()
                || !self.runtime.read(cx).external_approvals.pending().is_empty())
    }
}

impl SessionRuntime {
    /// The approval waiting on the person, as one line for the agent card
    /// (R3): a provider's own headline, or a plain ask for the server's.
    pub(crate) fn pending_approval_line(&self) -> Option<String> {
        if let Some(approval) = self.external_approvals.pending().last() {
            // What it would do, not just the tool's name ("Bash (Bash)" told
            // the person nothing, seen live 8 Nov): the approval block's own
            // command, when its card carries one.
            let command = self.store.session().and_then(|session| {
                session.turns.iter().rev().flat_map(|turn| turn.blocks().iter()).find_map(|block| match block {
                    // The block carries the provider's request id, not the
                    // tap's tool id: the newest still-pending block is it.
                    aui_protocol::Block::Approval { command, state: aui_protocol::ApprovalState::Pending, .. } if !command.trim().is_empty() => {
                        Some(command.trim().to_owned())
                    }
                    _ => None,
                })
            });
            return Some(match command {
                Some(command) => {
                    let short: String = if command.chars().count() > 200 { format!("{}…", command.chars().take(199).collect::<String>()) } else { command };
                    format!("{} — {short}", approval.headline)
                }
                None if !approval.reason.trim().is_empty() => format!("{} — {}", approval.headline, approval.reason.trim()),
                None => approval.headline.clone(),
            });
        }
        self.has_pending_approval_card().then(|| "This action needs your OK".to_owned())
    }

    /// The provider's pending approvals, `(id, headline)`, oldest first.
    pub(crate) fn pending_external_approvals(&self) -> Vec<(String, String)> {
        self.external_approvals.pending().iter().map(|a| (a.id.clone(), a.headline.clone())).collect()
    }

    /// Answer the provider's pending approval `id` (the agent card's
    /// question names the newest; a browser chat auto-accepts its own
    /// browser tools).
    pub(crate) fn answer_external(&mut self, id: &str, allow: bool, cx: &mut Context<Self>) {
        // The answer, in the one vocabulary: an allow takes Once, else This
        // session; a refusal takes Deny (docs/82 5.5).
        let wanted: &[provider::ApprovalAnswer] = if allow {
            &[provider::ApprovalAnswer::Once, provider::ApprovalAnswer::ThisSession]
        } else {
            &[provider::ApprovalAnswer::Deny]
        };
        // The tap's typed options first, then the inline card's own
        // choices — never a choice id guessed from which provider asked.
        let typed = self.external_approvals.get(id).and_then(|tap| {
            wanted.iter().find_map(|answer| tap.options.iter().find(|option| option.answer == *answer)).map(|option| option.choice.clone())
        });
        let own = typed.or_else(|| {
            self.store.session().and_then(|session| {
                session.turns.iter().rev().flat_map(|turn| turn.blocks().iter()).find_map(|block| match block {
                    aui_protocol::Block::Approval { id: block_id, choices, .. } if block_id == id => wanted.iter().find_map(|answer| {
                        choices
                            .iter()
                            .find(|c| provider::ApprovalAnswer::of(c.decision, c.scope) == Some(*answer))
                            .map(|c| c.id.clone())
                    }),
                    _ => None,
                })
            })
        });
        match own {
            Some(choice_id) => self.decide_provider_approval(id.to_owned(), choice_id, None, cx),
            None => crate::baaz_log!("approval {id}: offers no {} answer; nothing sent", if allow { "allowing" } else { "refusing" }),
        }
    }
}

impl SessionView {
    /// ⏎ on an empty composer with an approval pending (D4, the card's
    /// `⏎ allow` hint): the approving choice — the server's first Once /
    /// for-the-session choice, or a provider's Accept. Returns whether it
    /// answered one.
    pub fn allow_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.answer_pending(true, window, cx)
    }

    /// ⎋ with an approval pending (the card's `⎋ deny` hint): the refusing
    /// choice — the server's first Deny, or a provider's Decline.
    pub fn deny_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.answer_pending(false, window, cx)
    }

    fn answer_pending(&mut self, allow: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        use aui_protocol::ApprovalDecision as D;
        if self.card_field_open() {
            return false;
        }
        if let Some((_, choices)) = self.newest_pending_approval() {
            let wanted = |d: &D| if allow { matches!(d, D::Once | D::ApprovedForSession) } else { matches!(d, D::Deny) };
            let Some(index) = choices.iter().position(|c| wanted(&c.decision)) else { return false };
            // A press on a choice that exists: the step cannot fail here.
            let _ = self.choose(index + 1, window, cx);
            return true;
        }
        // A parked tap with no card: its typed options, in the vocabulary.
        let Some(tap) = self.runtime.read(cx).external_approvals.pending().last().map(|tap| tap.options.clone()) else {
            return false;
        };
        let wanted = if allow { provider::ApprovalAnswer::Once } else { provider::ApprovalAnswer::Deny };
        let Some(index) = tap.iter().position(|option| option.answer == wanted) else { return false };
        let _ = self.choose(index + 1, window, cx);
        true
    }

    /// Pick the n-th choice of the newest pending approval (`1`–`9`).
    pub fn choose_nth(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        // The same handler the `choose:<n>` step reaches, with the same
        // 1-based numbering the digits on the card carry. A digit past the
        // card's choices, or with nothing pending, presses nothing.
        let _ = self.choose(index + 1, window, cx);
    }
}

impl SessionRuntime {
    /// Whether plan mode is on, for the app's Shift+Tab.
    pub fn plan_mode(&self) -> bool {
        self.plan
    }
}

impl SessionView {
    /// What the application knows and the session does not: what the other
    /// sessions in this workspace are called (for a `ForkedFrom` marker).
    pub fn set_context(&mut self, titles: HashMap<String, String>) {
        self.titles = Rc::new(titles);
    }
}

impl SessionRuntime {
    /// The billing guard's banner, or `None` when the login's tier is fine.
    ///
    /// Pushed by the application, which owns the probe: the session view knows
    /// only what to draw and what to refuse. Muse lanes only — a provider
    /// session has no muse account behind it, so the banner never lands
    /// there and never gates its send.
    pub fn set_tier_banner(&mut self, banner: Option<TierBanner>, cx: &mut Context<Self>) {
        // The billing tier is Muse's account: no other provider's session
        // carries its banner or its guard, whichever lane it rides.
        if !self.provider_kind().has(provider::ProviderTrait::BillingTier) {
            return;
        }
        if self.tier_banner != banner {
            self.tier_banner = banner;
            cx.emit(RuntimeEvent::BannerChanged);
        }
    }
}

impl SessionView {
    /// "Send anyway" on the pay-as-you-go banner: the person accepts the
    /// bill for this run (the application keeps that, on
    /// [`SessionEvent::TierOverride`]) and the message the guard held back
    /// leaves now — pressing the button is the send, never a second Enter
    /// (2026-10-17: the draft stayed in the composer). The guard is lifted
    /// here first because the application's banner push lands only after
    /// this update; it would otherwise refuse the very send it allows.
    pub(crate) fn send_anyway(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(banner) = self.runtime.as_mut(cx).tier_banner.as_mut() {
            banner.blocking = false;
        }
        cx.emit(SessionEvent::TierOverride);
        // A refused send restores its words on the next frame; a press
        // before that frame sends them from here rather than from an
        // empty composer.
        let held = self.runtime.update(cx, |rt, _| rt.pending_prompt.take());
        if let Some(text) = held {
            self.dispatch_text(text, window, cx);
        } else if !self.draft_is_empty(cx) || !self.runtime.read(cx).images.is_empty() || !self.runtime.read(cx).files.is_empty() {
            self.send(window, cx);
        }
        cx.notify();
    }

    /// The floating composer band's painted height (see `bottom_inset`).
    pub fn set_bottom_inset(&mut self, height: f32, cx: &mut Context<Self>) {
        if (self.bottom_inset - height).abs() > 0.5 {
            self.keep_end_through(cx, |view| view.bottom_inset = height);
        }
    }

    /// The floating stack's measured height (see `stack_h`).
    pub(crate) fn note_stack_height(&mut self, height: f32, cx: &mut Context<Self>) {
        if (self.stack_h.get() - height).abs() > 0.5 {
            self.keep_end_through(cx, |view| view.stack_h.set(height));
        }
    }

    /// Changes the list's end inset without losing the end: a list that was
    /// at its end stays there. The inset arrives a frame after the first
    /// layout, so a transcript opened at its end used to sit one inset short
    /// of it, with "Jump to latest" up (owner 2026-10-15).
    ///
    /// Both callers measure in prepaint, inside a draw, where gpui-pre's
    /// `invalidate_view` records the view dirty but schedules nothing and
    /// the draw's end clears the dirty set: the notify below is lost, and
    /// the cached column keeps the inset it was built with until something
    /// else happens to notify it. A window whose frame source is stopped
    /// (display asleep, covered) builds its first column with a zero inset
    /// and never corrects it — the stack, its banner included, painted
    /// behind the composer (`child-lost-banner`, 2026-11-16). So the
    /// notify is also deferred to the end of the effect cycle, outside the
    /// draw, where it marks the view dirty and asks for a frame.
    fn keep_end_through(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Self)) {
        let at_end = self.list_state.is_scrolled_to_end().unwrap_or(true);
        change(self);
        if at_end {
            self.follow = true;
            self.list_state.scroll_to_end();
        }
        cx.notify();
        let view = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = view.update(cx, |_, cx| cx.notify());
        });
    }

    /// Whether the transcript stands at its end — the person can see the
    /// newest turn. An unmeasured list counts as at its end, like
    /// everywhere else that asks.
    pub(crate) fn at_transcript_end(&self) -> bool {
        self.list_state.is_scrolled_to_end().unwrap_or(true)
    }
}

impl SessionRuntime {
    /// Whether the transcript is still being read back from the provider
    /// (its end is not the session's end yet).
    pub(crate) fn history_loading(&self) -> bool {
        self.loading_history
    }
}

impl SessionView {
    /// Text size or density changed: every transcript row is now a
    /// different height, but the list keeps the heights it measured at the
    /// old sizes until something asks again, so a transcript read at its end
    /// ended under the composer — at Large the "Waiting for approval…" row
    /// sat hidden behind it (Gate 2, 16 Nov). Measure every row afresh and
    /// keep a reader at the end there.
    pub(crate) fn retype(&mut self, cx: &mut Context<Self>) {
        self.keep_end_through(cx, |this| this.list_state.remeasure());
    }

    /// Draw the cards settled, for a `--screenshot` run.
    pub fn set_at_rest(&mut self, at_rest: bool) {
        self.at_rest = at_rest;
    }
}

impl SessionRuntime {
    /// Whether this view folds a capture file rather than a live session.
    /// Replayed views are never parked in the session cache: their transcript
    /// is the file, and reopening re-reads it.
    pub fn is_replay(&self) -> bool {
        self.replay
    }
}

impl SessionView {
    /// Pin the tail after history landed. The application calls this on
    /// [`SessionEvent::HistoryReady`], which the backfill emits after its
    /// first page — not at the end — so later pages keep arriving under a
    /// pinned tail.
    pub fn follow_tail(&mut self, cx: &mut Context<Self>) {
        self.follow = true;
        cx.notify();
    }

    /// Move the transcript list (see [`ListJump`]) and redraw: the one
    /// place a click, a reported height, a step or the bench moves it.
    pub(crate) fn jump_list(&mut self, to: ListJump, cx: &mut Context<Self>) {
        match to {
            ListJump::Tail => {
                self.follow = true;
                self.list_state.scroll_to_end();
            }
            #[cfg(feature = "probe")]
            ListJump::End => self.list_state.scroll_to_end(),
            #[cfg(feature = "probe")]
            ListJump::Row(item_ix) => {
                self.follow = false;
                self.list_state.scroll_to(gpui::ListOffset { item_ix, offset_in_item: px(0.0) });
            }
        }
        cx.notify();
    }
}

/// Where [`SessionView::jump_list`] moves the transcript list.
pub(crate) enum ListJump {
    /// The end, following it from now on (the jump pill, a resize at the end).
    Tail,
    /// The end, following as before (the `end` step).
    #[cfg(feature = "probe")]
    End,
    /// Row `n` from the top, no longer following (the `top`/`mid` steps,
    /// the bench).
    #[cfg(feature = "probe")]
    Row(usize),
}

impl SessionRuntime {
    #[cfg(feature = "probe")]
    /// Fold a recording into this view: `--replay` (decision A0). The
    /// recording folds through the same translation its provider's live
    /// lane runs (its factory's `replay`, docs/82 5.7), at once and with no
    /// child and no wire, so screenshots cost nothing. The view follows the
    /// recording's own session id, learns the prompts its turns were sent
    /// with, and a turn the recording leaves open reads running.
    pub fn load_replay(&mut self, replay: provider::Replay, cx: &mut Context<Self>) {
        self.replay = true;
        if let Some(id) = replay.session_id {
            self.session_id = id;
            self.store.rename(&self.session_id);
        }
        for (command_id, text) in &replay.sent {
            self.remember_command(command_id, text);
        }
        self.fold_provider_events(replay.events, cx);
        self.observe_clocks(cx);
        cx.emit(RuntimeEvent::EntriesAppended);
    }

    #[cfg(feature = "probe")]
    /// Start a `--bench` run over a capture: read-only like a replay, but the
    /// events are fed one per cadence tick by the bench driver rather than
    /// folded all at once, so streaming cost is real.
    pub fn begin_bench_replay(&mut self, session_id: String, sent: Vec<(String, String)>, cx: &mut Context<Self>) {
        self.replay = true;
        self.session_id = session_id;
        self.store.rename(&self.session_id);
        for (command_id, text) in sent {
            self.remember_command(&command_id, &text);
        }
        cx.emit(RuntimeEvent::EntriesAppended);
    }
}

impl SessionView {
    #[cfg(feature = "probe")]
    /// Drive the transcript list to `frac` of the folded turns (0 = top,
    /// 1 = tail) without touching the pointer, for `--bench --bench-scroll`.
    pub fn bench_scroll_to(&mut self, frac: f32, cx: &mut Context<Self>) {
        // Rows, not turns: the list is one item per block. The cache may be
        // a frame behind the fold; the row count is what the list holds now.
        let len = self.list_len;
        let ix = ((len.saturating_sub(1) as f32) * frac.clamp(0.0, 1.0)) as usize;
        self.jump_list(ListJump::Row(ix), cx);
    }

    #[cfg(feature = "probe")]
    /// Pin the transcript to the tail, for `--bench --bench-scroll tail`.
    pub fn bench_scroll_tail(&mut self, cx: &mut Context<Self>) {
        self.jump_list(ListJump::Tail, cx);
    }

    #[cfg(feature = "probe")]
    /// The transcript list's current scroll offset, sampled once per frame by
    /// `--bench --bench-scroll wheel`.
    pub fn bench_list_top(&self) -> gpui::ListOffset {
        self.list_state.logical_scroll_top()
    }

    /// The transcript list's absolute scroll offset in pixels — the number a
    /// scrollbar would draw, and the only place the list exposes its
    /// logical-to-pixel mapping.
    ///
    /// [`Self::bench_list_top`] above is the list's *logical* anchor
    /// (`item_ix` + an offset inside that item), which cannot be compared
    /// across frames: the pixels an item is worth change as its 72 px hint is
    /// replaced by a real measurement. This is what a smoothness trace has to
    /// sample — a smooth scroll is a straight line here, and stepping is a
    /// staircase.
    pub fn bench_list_px(&self) -> f32 {
        f32::from(self.list_state.scroll_px_offset_for_scrollbar().y)
    }

    #[cfg(feature = "probe")]
    /// Whether the transcript list is pinned at its tail, sampled with the
    /// offset above: a wheel event that leaves the position unchanged *at*
    /// the limit is clamped, not stalled. Mirrors the app's own follow
    /// logic (`sync_virtual_list`, `render_needs_you`), which treats the
    /// unknown-height `None` as at the tail.
    pub fn bench_list_end(&self) -> bool {
        self.list_state.is_scrolled_to_end().unwrap_or(true)
    }

    /// An unsent composer's movable content: the text plus whatever is waiting
    /// to go out with the next turn. Moved between sessions when a draft is
    /// retargeted at another project, never cloned.
    pub(crate) fn take_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Draft {
        let draft = Draft {
            text: self.composer.read(cx).value().to_string(),
            images: self.runtime.update(cx, |rt, _| std::mem::take(&mut rt.images)),
            files: self.runtime.update(cx, |rt, _| std::mem::take(&mut rt.files)),
        };
        self.composer.update(cx, |state, cx| state.set_value(String::new(), window, cx));
        self.on_draft_changed(cx);
        draft
    }

    /// Receive a moved-in draft. The images and files land first so the
    /// composer's own write sees the state the next send will read.
    pub(crate) fn put_draft(&mut self, draft: Draft, window: &mut Window, cx: &mut Context<Self>) {
        self.runtime.as_mut(cx).images = draft.images;
        self.runtime.as_mut(cx).files = draft.files;
        self.set_draft(draft.text, window, cx);
        self.on_draft_changed(cx);
    }
}

impl SessionRuntime {
    /// The approval mode and effort this unsent draft stands on — what a
    /// provider switch carries into the draft that replaces it.
    pub(crate) fn draft_settings(&self) -> (PermissionMode, Option<ReasoningEffort>) {
        (self.mode(), self.effort)
    }

    /// Take a replaced draft's mode and effort (see [`Self::draft_settings`]):
    /// a fresh view queues the mode for its start like any pick, and the
    /// effort is the level the first turn asks for.
    pub(crate) fn carry_draft_settings(
        &mut self,
        mode: PermissionMode,
        effort: Option<ReasoningEffort>,
        cx: &mut Context<Self>,
    ) {
        if self.mode() != mode {
            self.set_mode(mode, cx);
        }
        self.set_initial_effort(effort, cx);
    }

    /// Whether a turn is in flight on this view: submitted and no
    /// `turn/started` yet, or the server says it runs. A sending view is no
    /// draft, even before its row exists.
    pub(crate) fn is_sending(&self) -> bool {
        self.submitting || self.running.is_some()
    }
}

impl SessionView {
    /// Whether this view holds no movable content: nothing typed, nothing
    /// attached. What decides a retargeted draft may land here without
    /// clobbering anything.
    pub(crate) fn draft_content_empty(&self, cx: &gpui::App) -> bool {
        self.runtime.read(cx).images.is_empty() && self.runtime.read(cx).files.is_empty() && self.composer.read(cx).value().trim().is_empty()
    }

    /// The composer's current text, for hosts that append to the draft
    /// rather than clobbering it.
    pub(crate) fn draft_text(&self, cx: &gpui::App) -> String {
        self.composer.read(cx).value().to_string()
    }

    /// The words the composer holds, or will hold on its next frame: a
    /// send handed back (`restore_prompt`) waits for a frame to land in the
    /// field, and a view closed before that frame must not lose it.
    pub(crate) fn words_in_hand(&self, cx: &gpui::App) -> String {
        self.runtime.read(cx).pending_prompt.clone().unwrap_or_else(|| self.draft_text(cx))
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

    /// The quote flow (docs/43 §0): add a quote block above the composer's
    /// text — quoting text already quoted adds nothing — and give the field
    /// the keyboard with the caret after any draft.
    pub(crate) fn add_quote(&mut self, text: String, source: Option<String>, code: Option<(String, usize)>, window: &mut Window, cx: &mut Context<Self>) {
        self.add_quote_with_span(text, source, code, None, window, cx);
    }

    /// [`Self::add_quote`] remembering the selection it came from.
    pub(crate) fn add_quote_with_span(
        &mut self,
        text: String,
        source: Option<String>,
        code: Option<(String, usize)>,
        span: Option<(String, aui::transcript::MessageSelection)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = text.trim_end().to_string();
        if text.is_empty() || self.runtime.read(cx).quotes.iter().any(|q| q.text == text) {
            self.focus_composer(window, cx);
            return;
        }
        self.runtime.update(cx, |rt, _| {
            rt.quote_seq += 1;
            rt.quotes.push(QuoteDraft { id: format!("quote-{}", rt.quote_seq), text, source, code, span });
        });
        let draft = self.draft_text(cx);
        let end = position_of(&draft, draft.len());
        self.composer.update(cx, |state, cx| state.set_cursor_position(end, window, cx));
        self.focus_composer(window, cx);
        cx.notify();
    }

    /// Move the keyboard to the composer.
    pub fn focus_composer(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.composer.focus_handle(cx), cx);
    }

    /// Whether the keyboard is in the composer: ⏎ there sends, whatever
    /// card field stands open above it.
    pub fn composer_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.composer.focus_handle(cx).is_focused(window)
    }

    /// The composer's focus handle: where closing one of its menus hands the
    /// keyboard back (and what the browser-focus tests check).
    pub(crate) fn composer_focus_handle(&self, cx: &mut Context<Self>) -> FocusHandle {
        self.composer.focus_handle(cx).clone()
    }
}

impl SessionRuntime {
    /// The view opened ahead of its `session/start`: sends and picks wait
    /// for it (see [`StartQueue`]).
    pub(crate) fn begin_start(&mut self) {
        self.start = StartState::Starting(StartQueue::default());
    }

    /// Whether this view's `session/start` (or provider open) is still on
    /// the wire.
    pub(crate) fn is_starting(&self) -> bool {
        matches!(self.start, StartState::Starting(_))
    }

    /// Whether an approval card in a turn that is still open waits on the
    /// person, read from the fold (the view's render cache holds the same
    /// answer for its keys; the runtime cannot borrow a view's cache).
    pub(crate) fn has_pending_approval_card(&self) -> bool {
        self.session().is_some_and(|session| {
            session.turns.iter().rev().any(|turn| {
                !self.turn_over(turn.id())
                    && turn.blocks().iter().any(|block| {
                        matches!(block, Block::Approval { state: aui_protocol::ApprovalState::Pending, .. })
                    })
            })
        })
    }

    /// Whether shutting this session down would lose work: a turn in
    /// flight, an approval or question waiting on the person, or a start
    /// still on the wire. Eviction never shuts such a runtime down — it
    /// may drop the view, never the runtime (docs/81 A F1).
    pub(crate) fn must_stay_alive(&self) -> bool {
        self.is_sending()
            || self.is_starting()
            || self.has_pending_approval_card()
            || !self.external_approvals.pending().is_empty()
            || !self.external_questions.is_empty()
            || self.newest_pending_question().is_some()
    }

    /// The banner's text, for tests that check what a failure said.
    #[cfg(test)]
    pub(crate) fn banner_text(&self) -> Option<&str> {
        self.banner.as_deref()
    }

    /// The start's queue, while it is on the wire.
    fn start_queue(&mut self) -> Option<&mut StartQueue> {
        match &mut self.start {
            StartState::Starting(queue) => Some(queue),
            _ => None,
        }
    }
}

impl SessionRuntime {
    /// The start landed: the session is real, and whatever the person did
    /// while it was on the wire goes now, in order — the picks first, so
    /// the sends run under them. Returns the sends, each exactly as typed,
    /// for the view to dispatch (a typed `/command` needs its window);
    /// `None` when no start was pending.
    pub(crate) fn take_start(&mut self, cx: &mut Context<Self>) -> Option<Vec<String>> {
        let StartState::Starting(queue) = std::mem::replace(&mut self.start, StartState::Started) else {
            return None;
        };
        // A mode picked while opening (the fallback out of Auto among
        // them) stands before the model replays, so the replayed model
        // pick reads it rather than the open's request.
        if let Some(mode) = queue.mode {
            self.stand_on_mode(mode);
        }
        if let Some(model) = queue.model {
            self.set_model(&model, cx);
        }
        if let Some(mode) = queue.mode {
            self.set_mode(mode, cx);
        }
        if queue.models {
            self.load_models(cx);
        }
        if let Some(on) = queue.plan {
            self.send_native_plan(on, cx);
        }
        // The busy state the queued sends held is theirs to set again:
        // a send refused on landing (the billing guard) must not leave it.
        if !queue.sends.is_empty() {
            self.submitting = false;
        }
        Some(queue.sends)
    }
}

impl SessionView {
    /// The start landed ([`SessionRuntime::take_start`]): each send made
    /// while it was on the wire goes now, exactly as typed, and the view
    /// redraws on the landed state.
    pub(crate) fn start_landed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sends = self.runtime.update(cx, |rt, cx| rt.take_start(cx)).unwrap_or_default();
        for text in sends {
            self.dispatch_text(text, window, cx);
        }
        cx.notify();
    }
}

impl SessionRuntime {
    /// The start failed: there is no session behind this view, so it
    /// refuses every command from here on and says why. The draft stays,
    /// and the sends that waited go back into it.
    pub(crate) fn start_failed(&mut self, message: String, cx: &mut Context<Self>) {
        if let StartState::Starting(queue) = std::mem::replace(&mut self.start, StartState::Failed) {
            if !queue.sends.is_empty() {
                self.submitting = false;
                self.restore_prompt(queue.sends.join("\n\n"), cx);
            }
        }
        self.plain_banner(format!("This session did not start: {message}"), cx);
    }

    /// Lock the composer while a provider switch replaces this view (Y2b).
    pub(crate) fn set_input_locked(&mut self, locked: bool, cx: &mut Context<Self>) {
        self.input_locked = locked;
        cx.emit(RuntimeEvent::ComposerChanged);
    }

    /// Whether sends are refused while the replacement opens.
    pub(crate) fn is_input_locked(&self) -> bool {
        self.input_locked
    }

    /// Show the switch's pick on the chip while this view is still drawn.
    pub(crate) fn set_switching_to(&mut self, provider: Option<ProviderId>, cx: &mut Context<Self>) {
        self.switching_to = provider;
        cx.emit(RuntimeEvent::ComposerChanged);
    }

    /// The new-session row's facts (docs/82 6.6), or `None` to hide it.
    /// The window sets them on every activation.
    pub(crate) fn set_new_row(&mut self, row: Option<pickers::NewRow>, cx: &mut Context<Self>) {
        if self.new_row != row {
            self.new_row = row;
            cx.emit(RuntimeEvent::ComposerChanged);
        }
    }

    /// The new-session row's facts, when this session shows it: set for
    /// an unsent draft, and only while no turn has started or is moving.
    pub(crate) fn new_row(&self) -> Option<&pickers::NewRow> {
        self.new_row.as_ref().filter(|_| !self.has_turns() && !self.is_sending())
    }

    /// Whether the draft is moving (a switch's replacement opening): the
    /// new-session row's controls wait.
    pub(crate) fn is_moving(&self) -> bool {
        self.switching_to.is_some()
    }

    /// The provider mark the composer chip draws (Y2b display override).
    pub(crate) fn display_provider(&self) -> ProviderId {
        self.switching_to.unwrap_or_else(|| self.provider_kind())
    }
}

/// A lane whose child died twice in a row, typed (docs/71 §2–§3): what the
/// grown lost banner says and offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChildFailure {
    /// What went wrong, typed: the sentence and the action follow it.
    pub kind: crate::provider_status::FailureKind,
    /// The last loss, in the adapter's words.
    pub message: String,
    /// The child's last stderr lines (at most
    /// [`provider::STDERR_TAIL_LINES`]), oldest first.
    pub stderr_excerpt: Vec<String>,
}

/// What the inline banner's action does, when the failure is one the person can
/// do something about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BannerAction {
    /// Resend a turn that never left, with the text it carried.
    RetryTurn(String),
    /// Re-run a user shell command that never left.
    RetryShell(String),
    /// Open Settings → Providers (a session on a disabled provider).
    OpenProviders,
    /// Revive a lane whose child died, now, without sending a turn
    /// ("Retry now", docs/71 §2).
    RetryNow,
    /// The shared-folder warning (docs/82 6.2): another session has a turn
    /// running in this session's folder. `worktree` when the session is an
    /// unsent draft that can move into a worktree of its own ("Run in a
    /// worktree"); otherwise the banner only dismisses.
    SharedFolder {
        /// The draft can move into a worktree.
        worktree: bool,
    },
}

/// The shared-folder warning's words (docs/82 6.2). No provider name
/// (rule 14).
pub(crate) const SHARED_FOLDER_TEXT: &str = "Another session is working in this folder. Edits from both will mix.";


/// The pending question a block id names, resolved once so the callers do not
/// each re-walk the fold.
struct Pending {
    input_id: String,
    question_id: String,
    multi: bool,
    labels: Vec<String>,
    questions: usize,
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
fn pressure(level: provider::facts::ContextPressure) -> ContextPressure {
    match level {
        provider::facts::ContextPressure::Warning => ContextPressure::Warning,
        provider::facts::ContextPressure::Blocked => ContextPressure::Blocked,
        provider::facts::ContextPressure::Normal => ContextPressure::Normal,
    }
}

#[cfg(feature = "probe")]
/// `used/window/pressure`, the `--steps context:` payload.
fn parse_context(spec: &str) -> Option<provider::facts::ContextReading> {
    let mut parts = spec.split('/');
    let used: u64 = parts.next()?.parse().ok()?;
    let window: u64 = parts.next()?.parse().ok()?;
    let pressure = match parts.next().unwrap_or("normal") {
        "warning" => provider::facts::ContextPressure::Warning,
        "blocked" => provider::facts::ContextPressure::Blocked,
        _ => provider::facts::ContextPressure::Normal,
    };
    Some(provider::facts::ContextReading { pressure, used_tokens: used, window_tokens: (window > 0).then_some(window) })
}

/// Page a session's whole view forward, from the beginning, into fold events.
///
/// Blocking: every call waits on the wire, so this runs on the background
/// executor. A page that comes back empty, or a `nextCursor` of `null`, is the
/// end of the view in that direction.
/// Whether element-construction timing is recorded: `BAAZ_FRAME_STATS=1`
/// or `--bench`, which implies it. Read once (A-MECH-14): the flag never
/// changes at runtime, and the old code paid an env lookup per frame.
fn frame_stats_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("BAAZ_FRAME_STATS").as_deref() == Ok("1")
            || BENCH_FRAMES.load(std::sync::atomic::Ordering::Relaxed)
    })
}

#[cfg(feature = "probe")]
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
/// session: long `BAAZ_FRAME_STATS` runs pay no timestamp vec).
static FRAME_TIMES: std::sync::OnceLock<std::sync::Mutex<Vec<std::time::Instant>>> = std::sync::OnceLock::new();

static FRAME_SAMPLE_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Debug-only frame timer (C1/P1): with `BAAZ_FRAME_STATS=1`, record
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
            "baaz-frame-stats n={} p50={}us p90={}us p99={}us max={}us",
            sorted.len(),
            at(0.5),
            at(0.9),
            at(0.99),
            sorted[sorted.len() - 1]
        );
        samples.clear();
    }
}

#[cfg(feature = "probe")]
/// How many `render_transcript` constructions have been recorded. Every frame
/// renders the transcript, so the delta over a quiet window is the frame
/// count there — the bench's idle assertion.
pub(crate) fn frame_sample_count() -> u64 {
    FRAME_SAMPLE_COUNT.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(feature = "probe")]
/// Drain the recorded element-construction samples (microseconds), for the
/// bench's end-of-run table.
pub(crate) fn take_frame_samples() -> Vec<u128> {
    let samples = FRAME_SAMPLES.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    samples.lock().map(|mut samples| std::mem::take(&mut *samples)).unwrap_or_default()
}

#[cfg(feature = "probe")]
/// Drain the recorded paint timestamps, for the bench's frame-interval table.
pub(crate) fn take_frame_times() -> Vec<std::time::Instant> {
    let times = FRAME_TIMES.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    times.lock().map(|mut times| std::mem::take(&mut *times)).unwrap_or_default()
}

/// Whole-frame timing (`bench-draw`): the root render
/// stamps the start; a zero-size canvas painted last in the same frame
/// stamps the end. Paint order is depth-first, so the trailing child closes
/// the frame's own paint — `window.on_next_frame` would not: gpui runs
/// `next_frame_callbacks` at the top of the *next* request-frame.
static DRAW_START: std::sync::OnceLock<std::sync::Mutex<Option<Instant>>> = std::sync::OnceLock::new();
static DRAW_SAMPLES: std::sync::OnceLock<std::sync::Mutex<Vec<u128>>> = std::sync::OnceLock::new();

/// Stamp the start of a whole-frame draw. First line of the root render
/// (`Harness::render`, `BenchRoot::render`); recorded only while frame stats
/// are on (`BAAZ_FRAME_STATS=1` or `--bench`), so a disabled build pays
/// one relaxed load per frame.
/// The previous frame's render-to-paint micros, for the frame trace's
/// `draw_us` column. Written by the paint marker below, read by the next
/// traced paint; zero until the first paint lands.
static LAST_DRAW_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn last_draw_us() -> u64 {
    LAST_DRAW_US.load(std::sync::atomic::Ordering::Relaxed)
}

pub(crate) fn note_draw_start() {
    if !(frame_stats_enabled() || frame_trace_enabled()) {
        return;
    }
    if let Ok(mut start) = DRAW_START.get_or_init(|| std::sync::Mutex::new(None)).lock() {
        *start = Some(Instant::now());
    }
}

/// Close the frame stamped above: a zero-size canvas, painted last in the
/// root, whose paint closure records the render-to-paint duration. It paints
/// nothing and takes no space, so captures are unaffected. A render with no
/// paint behind it (a hidden window) leaves a start the next render
/// overwrites, so `bench-draw` may count fewer frames than `bench-element`.
pub(crate) fn draw_end_marker() -> AnyElement {
    gpui::canvas(|_, _, _| (), |_, (), _, _| {
        note_draw_end();
    })
    .into_any_element()
}

fn note_draw_end() {
    if !(frame_stats_enabled() || frame_trace_enabled()) {
        return;
    }
    let start = DRAW_START
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .map(|mut start| start.take())
        .unwrap_or(None);
    if let Some(start) = start {
        let micros = start.elapsed().as_micros();
        LAST_DRAW_US.store(micros.min(u128::from(u64::MAX)) as u64, std::sync::atomic::Ordering::Relaxed);
        // The bench's end-of-run table only: a trace-only run must not grow
        // this vec forever.
        if frame_stats_enabled() {
            if let Ok(mut samples) = DRAW_SAMPLES.get_or_init(|| std::sync::Mutex::new(Vec::new())).lock() {
                samples.push(micros);
            }
        }
    }
}

#[cfg(feature = "probe")]
/// Drain the recorded whole-frame samples (microseconds), for the bench's
/// end-of-run table.
pub(crate) fn take_draw_samples() -> Vec<u128> {
    let samples = DRAW_SAMPLES.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    samples.lock().map(|mut samples| std::mem::take(&mut *samples)).unwrap_or_default()
}

/// Whether the per-tick frame trace is on: `BAAZ_FRAME_TRACE=1` in the
/// normal window, read once. The trace is how a hand gesture on `--replay`
/// becomes a measurement: every root render appends one row
///, and `scripts/frame-trace.py`
/// summarises the log. `pub(crate)` so [`crate::sidebar_view`] can gate its
/// own trace-only counter the same way.
pub(crate) fn frame_trace_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("BAAZ_FRAME_TRACE").as_deref() == Ok("1"))
}

/// How long after the last applied wheel event a traced paint still counts
/// as mid-gesture: the momentum tail arrives at 60 Hz, so 150 ms covers a
/// missed sample.
const TRACE_GESTURE_MS: u128 = 150;

/// How long after a wheel event its gesture stays open:
/// the momentum tail arrives at 60 Hz, so 150 ms covers a missed sample.
/// Deliberately the same horizon the frame trace displays above.
const GESTURE_HORIZON: Duration = Duration::from_millis(150);

/// Wheel drains actually applied since process start:
/// one per frame that had accumulated travel, against one `scroll_by` per
/// event before. The bench drains it for its report; the count is what shows
/// per-frame work no longer growing with the event rate.
static WHEEL_SCROLL_BYS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(feature = "probe")]
/// Drain the applied-drain count above, for the bench's end-of-run report.
pub(crate) fn take_wheel_scroll_bys() -> u64 {
    WHEEL_SCROLL_BYS.swap(0, std::sync::atomic::Ordering::SeqCst)
}

/// Hero vs loading paints since process start: what the
/// empty transcript showed, per render. Relaxed atomics, drained per
/// `centre` log line, so a scripted open's first frames read as
/// `hero=0 loading=N` (fixed) rather than `hero=N` (the flicker).
static CENTRE_HERO_PAINTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static CENTRE_LOADING_PAINTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Count one new-session hero paint, for the `centre` report.
pub(crate) fn note_centre_hero_paint() {
    CENTRE_HERO_PAINTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Count one loading-row paint, for the `centre` report.
pub(crate) fn note_centre_loading_paint() {
    CENTRE_LOADING_PAINTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(feature = "probe")]
/// Drain both counts above, for the `centre` report.
pub(crate) fn take_centre_paints() -> (u64, u64) {
    (
        CENTRE_HERO_PAINTS.swap(0, std::sync::atomic::Ordering::Relaxed),
        CENTRE_LOADING_PAINTS.swap(0, std::sync::atomic::Ordering::Relaxed),
    )
}

/// Centre builds since process start: how many
/// times the cached centre actually rebuilt, per render. Relaxed atomic,
/// drained per `sbwheel` log line, so a sidebar wheel burst reads
/// `centre=0` (reused) while typing, streaming and switching bump it.
static CENTRE_RENDERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Count one centre build, for the `sbwheel` report.
pub(crate) fn note_centre_render() {
    CENTRE_RENDERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if frame_trace_enabled() {
        TRACE_CENTRE_TICKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(feature = "probe")]
/// Drain the count above, for the `sbwheel` report.
pub(crate) fn take_centre_renders() -> u64 {
    CENTRE_RENDERS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// Centre builds since the last traced root tick:
/// a `sbwheel`/`centre` drain and the frame trace's own row must not fight
/// over the same counter, so this one exists solely for
/// [`note_root_frame_trace`] and is never touched by the step instruments.
static TRACE_CENTRE_TICKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Drain the trace-only centre count above, for one frame-trace row.
fn take_trace_centre_ticks() -> u64 {
    TRACE_CENTRE_TICKS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

struct FrameTrace {
    out: std::io::BufWriter<std::fs::File>,
    start: Instant,
}

/// The open trace file, if the trace is on and the log could be opened:
/// `$BAAZ_STATE_DIR/frame-trace.log`, falling back to Baaz's own
/// `~/Library/Application Support/baaz/`. One handle per process,
/// appended to, flushed per paint so a crash keeps what painted.
fn frame_trace_slot() -> Option<std::sync::MutexGuard<'static, Option<FrameTrace>>> {
    if !frame_trace_enabled() {
        return None;
    }
    static TRACE: std::sync::OnceLock<std::sync::Mutex<Option<FrameTrace>>> = std::sync::OnceLock::new();
    let slot = TRACE.get_or_init(|| {
        std::sync::Mutex::new(
            (|| {
                let dir = crate::store::support_dir();
                std::fs::create_dir_all(&dir).ok()?;
                let file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join("frame-trace.log"))
                    .ok()?;
                let mut out = std::io::BufWriter::new(file);
                use std::io::Write as _;
                let _ = writeln!(
                    out,
                    "# baaz frame trace v3: t_us,root,pane,centre,sidebar_ix,sidebar_off,sidebar_w,resize_active,list_px,events_since_last_tick,gesture_active,rehint,draw_us"
                );
                Some(FrameTrace { out, start: Instant::now() })
            })(),
        )
    });
    slot.lock().ok()
}

impl SessionView {
    /// Accumulate one capture-phase wheel delta. The
    /// handler applies nothing: `render_transcript` drains the sum into one
    /// `scroll_by` per frame. An upward delta leaves the tail, as everywhere
    /// else; every event re-arms the gesture horizon for the momentum tail.
    pub(crate) fn push_wheel(&mut self, dy: Pixels) {
        self.pending_wheel += dy;
        if dy > px(0.0) {
            self.follow = false;
        }
        self.gesture_until = Some(Instant::now() + GESTURE_HORIZON);
        self.note_wheel_event();
    }

    /// Whether a wheel gesture is in flight: an event
    /// landed within the horizon. While this holds the transcript presents
    /// every tick and neither re-hints nor re-anchors under the gesture.
    fn gesture_active(&self) -> bool {
        self.gesture_until.is_some_and(|until| Instant::now() < until)
    }

    /// Drain the accumulated wheel travel into exactly one `scroll_by`.
    /// Called once per frame from `render_transcript`,
    /// before the cache sync, so N events between paints become one SumTree
    /// seek with their full travel. A zero delta is gpui's own early-return,
    /// so an idle frame pays one comparison; only applied drains count.
    /// `pub(crate)` for the `--steps wheel:` verb, which applies eagerly so
    /// its before/after log stays a movement check.
    pub(crate) fn drain_pending_wheel(&mut self) {
        let pending = std::mem::replace(&mut self.pending_wheel, px(0.0));
        self.list_state.scroll_by(-pending);
        if pending != px(0.0) {
            WHEEL_SCROLL_BYS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Count one applied wheel event for the frame trace. Called from the
    /// capture handler's view update, beside the `follow` bookkeeping.
    fn note_wheel_event(&mut self) {
        if !frame_trace_enabled() {
            return;
        }
        self.trace_wheel_pending += 1;
        self.trace_last_wheel = Some(Instant::now());
    }

    /// Count one applied re-hint for the frame trace. Called beside every
    /// `rehint_rows` run; drained by the next traced paint.
    fn note_trace_rehint(&mut self) {
        if frame_trace_enabled() {
            self.trace_rehint = true;
        }
    }

    /// Take the traced re-hint flag above, for the resize sweep's per-tick
    /// log. A resize tick that re-hints re-measures its visible rows; one
    /// that does not rides the old measurements.
    pub(crate) fn take_trace_rehint(&mut self) -> bool {
        std::mem::take(&mut self.trace_rehint)
    }

    /// Peek and drain this session's per-tick trace fields: the transcript list's pixel offset, the wheel events
    /// applied since the last traced tick, and whether a wheel gesture is
    /// still active. Replaces the old centre-scoped `note_frame_trace`
    /// (v2), which only ever fired when the cached transcript column itself
    /// rebuilt — silent through a sidebar-only or resize-only tick since
    /// those stopped touching the centre at all. Called once per root
    /// render from `Harness::trace_root_frame`, not
    /// from `render_transcript`, so it sees every tick regardless of which
    /// subtree actually rebuilt.
    pub(crate) fn trace_tick(&mut self) -> (f32, usize, bool) {
        let list_px = self.bench_list_px();
        let events = std::mem::take(&mut self.trace_wheel_pending);
        let gesture = self.trace_last_wheel.is_some_and(|at| at.elapsed().as_millis() < TRACE_GESTURE_MS);
        (list_px, events, gesture)
    }
}

/// Append one frame-trace row (v3 format — see the header line
/// `frame_trace_slot` writes): micros since the trace opened;
/// `root` (always 1 — one row is written per `Harness::render`, so this
/// documents the tick model rather than measuring anything); `pane` and
/// `centre`, how many times the sidebar pane and the cached transcript
/// column actually rebuilt since the last row (0 most ticks — that they
/// stay 0 during a sidebar-only or resize-only gesture is the point of
/// that isolation, and this column is how a regression would show up);
/// the sidebar list's `ListOffset` (`sidebar_ix`+`sidebar_off`, same shape
/// as the `sbwheel` step's log); the sidebar divider's current width;
/// `resize_active`, whether a divider drag is in flight this tick; the
/// transcript list's pixel offset and wheel events applied since the last
/// row (0/0 when no session is open); `gesture`, the OR of all three
/// interactions' own gesture flags (transcript wheel, sidebar wheel,
/// resize drag) — one column `scripts/frame-trace.py` can use for any of
/// the three measured gestures without knowing which one is live; whether
/// a transcript re-hint ran; and the previous frame's render-to-paint
/// micros ([`note_draw_start`]/[`draw_end_marker`], unchanged by this
/// split).
///
/// Called once from `Harness::trace_root_frame`, which runs first in
/// `Harness::render` — i.e. once per display tick that renders anything at
/// all, matching the zero-idle-frames rule: an idle window writes no rows.
#[allow(clippy::too_many_arguments)]
pub(crate) fn note_root_frame_trace(
    sidebar_ix: usize,
    sidebar_off: f32,
    sidebar_w: f32,
    resize_active: bool,
    transcript: Option<(f32, usize, bool)>,
    sidebar_gesture: bool,
    rehint: bool,
) {
    let Some(mut slot) = frame_trace_slot() else { return };
    let Some(trace) = slot.as_mut() else { return };
    let pane = crate::sidebar_view::take_trace_pane_ticks();
    let centre = take_trace_centre_ticks();
    let (list_px, events, transcript_gesture) = transcript.unwrap_or((0.0, 0, false));
    let gesture = transcript_gesture || sidebar_gesture || resize_active;
    let draw_us = last_draw_us();
    use std::io::Write as _;
    let _ = writeln!(
        trace.out,
        "{},1,{},{},{},{:.1},{:.1},{},{:.1},{},{},{},{}",
        trace.start.elapsed().as_micros(),
        pane,
        centre,
        sidebar_ix,
        sidebar_off,
        sidebar_w,
        resize_active as u8,
        list_px,
        events,
        gesture as u8,
        rehint as u8,
        draw_us
    );
    let _ = trace.out.flush();
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

    fn assistant_turn(blocks: Vec<Block>) -> Turn {
        Turn::Assistant { id: "t-1".to_owned(), blocks, meta: aui_protocol::TurnMeta::default(), timestamp: None }
    }

    #[test]
    fn a_complete_two_block_reply_counts_as_complete() {
        let turn = assistant_turn(vec![
            Block::Text { text: "first".into(), streaming: false },
            Block::Text { text: "second".into(), streaming: false },
        ]);
        assert!(reply_complete(&turn));
    }

    #[test]
    fn a_streaming_text_block_is_still_working() {
        let turn = assistant_turn(vec![
            Block::Text { text: "half".into(), streaming: true },
            Block::Text { text: "done".into(), streaming: false },
        ]);
        assert!(!reply_complete(&turn));
    }

    #[test]
    fn a_working_activity_group_is_still_working() {
        let turn = assistant_turn(vec![
            Block::Text { text: "done".into(), streaming: false },
            Block::Activity {
                steps: Vec::new(),
                summary: String::new(),
                elapsed_ms: 0,
                state: ActivityState::Working,
            },
        ]);
        assert!(!reply_complete(&turn));
    }

    #[test]
    fn a_user_turn_is_never_a_complete_reply() {
        let turn = Turn::User {
            id: "u-1".into(),
            text: "hi".into(),
            attachments: Vec::new(),
            mentions: Vec::new(),
            timestamp: None,
        };
        assert!(!reply_complete(&turn));
    }

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
        assert_eq!(ROW_HEIGHT_HINT, 72.0);
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
        hinted.reset_with_uniform_height(592, px(ROW_HEIGHT_HINT));
        hinted.scroll_to_end();
        hinted.scroll_by(px(-120.0));
        // 120 px over 72 px rows: two rows up from the past-end anchor.
        let climbed = (120.0 / ROW_HEIGHT_HINT).ceil() as usize;
        assert_eq!(hinted.logical_scroll_top().item_ix, 592 - climbed);
    }

    /// Rows appended after the first fill — a history page — used to arrive
    /// with no hint, so the
    /// H2 stack came back on any session longer than one page. Re-hinting
    /// keeps measured heights (they become their own hints) and gives the
    /// new rows the row hint, and the tail pin survives it.
    #[test]
    fn appended_rows_are_hinted_and_the_tail_survives() {
        let list = ListState::new(0, ListAlignment::Top, px(crate::WINDOW_H));
        list.reset_with_uniform_height(10, px(ROW_HEIGHT_HINT));
        list.splice(10..10, 1000);
        list.scroll_to_end();
        // Unhinted: the sum tree above the tail is 10 hinted rows tall, so a
        // flick of 3 000 px lands at the head.
        list.scroll_by(px(-3000.0));
        assert_eq!(list.logical_scroll_top().item_ix, 0);
        // Re-hinted (what `rehint_rows` does): the same flick climbs about
        // 3000 / ROW_HEIGHT_HINT rows from the tail.
        list.reset_with_uniform_height(1010, px(ROW_HEIGHT_HINT));
        list.scroll_to_end();
        list.scroll_by(px(-3000.0));
        let expected = 1010 - (3000.0 / ROW_HEIGHT_HINT).ceil() as usize;
        assert!((expected.saturating_sub(1)..=expected + 1).contains(&list.logical_scroll_top().item_ix));
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

    /// B9: a reopened muse session chips its model, never the bare
    /// provider id. The fold names no model on a reopen, so the chip reads
    /// the session's own past through the `model/list` catalog: a known
    /// model id shows the row's display label, an unknown id shows the id
    /// humanised, and with nothing known yet the chip still never wears
    /// the lowercase wire id.
    #[gpui::test]
    fn muse_chip_names_the_model_never_the_provider_id(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;

        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let view = vc.update(|window, cx| {
            let host = SessionHost {
                provider_id: "muse".to_owned(),
                workspace: "/tmp/b9-muse-chip".to_owned(),
                overlays: cx.new(|_| crate::overlays::Overlays::default()),
                capture: crate::runtime::CaptureToken::default(),
                terminal_host: None,
            };
            cx.new(|cx| SessionView::new("s-b9".to_owned(), host, window, cx))
        });
        let row = |id: &str, label: &str| ModelCatalogEntry {
            context_limit: None,
            description: None,
            display_label: label.to_owned(),
            is_active: false,
            is_default: false,
            model_id: id.to_owned(),
            provider_id: "muse".to_owned(),
            ..Default::default()
        };
        // A reopened session: the fold names no model, the session's own
        // past names a catalog row — the chip reads the row's label.
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.runtime().as_mut(cx).models = vec![row("muse-everyday", "Muse Everyday")];
                view.runtime().as_mut(cx).history_model = Some("muse-everyday".to_owned());
            });
        });
        vc.update(|_, cx| {
            assert_eq!(
                view.read(cx).rt(cx).model().to_string(),
                "Muse Everyday",
                "a known muse model chips its catalog label"
            );
        });
        // A model the catalog omits chips humanised — never the provider.
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.runtime().as_mut(cx).history_model = Some("muse-quantum-2".to_owned());
            });
        });
        vc.update(|_, cx| {
            let chip = view.read(cx).rt(cx).model().to_string();
            assert_eq!(chip, "Muse Quantum 2", "an unknown muse model chips humanised");
            assert_ne!(chip, "muse", "the chip never wears the bare provider id");
        });
        // Nothing known yet: the chip still never wears the wire id.
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.runtime().as_mut(cx).models = Vec::new();
                view.runtime().as_mut(cx).history_model = None;
            });
        });
        vc.update(|_, cx| {
            let chip = view.read(cx).rt(cx).model().to_string();
            assert_eq!(chip, "Muse", "with no model known the chip names the human label");
            assert_ne!(chip, "muse", "the chip never wears the bare provider id");
        });
    }

    /// B2: the model menu checks the row the chip names, not the catalog's
    /// `is_active` row — and checks nothing when the chip names no row.
    /// The catalog flags row 0 active while the chip reads row 1; the menu
    /// must check row 1. A chip no row matches leaves every row unchecked.
    #[gpui::test]
    fn the_model_menu_checks_the_row_the_chip_names(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;

        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let view = vc.update(|window, cx| {
            let host = SessionHost {
                provider_id: "muse".to_owned(),
                workspace: "/tmp/b2-model-menu".to_owned(),
                overlays: cx.new(|_| crate::overlays::Overlays::default()),
                capture: crate::runtime::CaptureToken::default(),
                terminal_host: None,
            };
            cx.new(|cx| SessionView::new("s-b2".to_owned(), host, window, cx))
        });
        let row = |id: &str, label: &str, active: bool| ModelCatalogEntry {
            context_limit: None,
            description: None,
            display_label: label.to_owned(),
            is_active: active,
            is_default: false,
            model_id: id.to_owned(),
            provider_id: "muse".to_owned(),
            ..Default::default()
        };
        // The catalog's active row is stale: the chip names row 1.
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.runtime().as_mut(cx).models =
                    vec![row("muse-first", "Muse First", true), row("muse-second", "Muse Second", false)];
                view.runtime().as_mut(cx).pending_model = Some("muse-second".to_owned());
            });
        });
        vc.update(|_, cx| {
            assert_eq!(view.read(cx).rt(cx).model_id(), "muse-second");
            assert_eq!(view.read(cx).rt(cx).model().to_string(), "Muse Second");
            assert_eq!(view.read(cx).rt(cx).model_menu_checked(), Some(1));
        });
        // Opening the picker checks the chip's row, not the active one.
        // (Offline the catalog fetch is a no-op, so the rows above stand.)
        vc.update(|_, cx| {
            view.update(cx, |view, cx| view.toggle_picker(MenuKind::Model, cx));
        });
        vc.update(|_, cx| {
            let selected = view.read(cx).model_picker(cx).map(|picker| picker.read(cx).selected());
            assert_eq!(selected, Some(1), "the menu checks the row the chip names");
            view.update(cx, |view, cx| view.close_menu(cx));
        });
        // A chip no row matches: nothing is checked, never the first row.
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.runtime().as_mut(cx).pending_model = Some("muse-ghost".to_owned());
            });
        });
        vc.update(|_, cx| {
            assert_eq!(view.read(cx).rt(cx).model_menu_checked(), None);
        });
        vc.update(|_, cx| {
            view.update(cx, |view, cx| view.toggle_picker(MenuKind::Model, cx));
        });
        vc.update(|_, cx| {
            let selected = view.read(cx).model_picker(cx).map(|picker| picker.read(cx).selected());
            assert_eq!(selected, Some(2), "with no match no row is checked");
        });
    }
}
