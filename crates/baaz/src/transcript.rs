//! Turning the folded session into transcript elements.
//!
//! Everything here is a pure function of `aui_protocol` data: a [`Turn`] or a
//! [`Block`] in, an element out, with the collapsed/expanded state of the
//! foldable cards handed in by the view and toggles handed back through one
//! closure. No component in here holds state or touches the wire — the fold is
//! the only source of truth, and the view re-renders it whole every frame.
//!
//! The approval and question cards are **live**: a choice
//! becomes `approval/decide`, an answer becomes `userInput/answer`, and both go
//! out through [`Cards`], which the session view fills in. A replayed capture
//! wires them up too — opening a preview and picking an option are local, and a
//! command that would reach the wire is refused with a banner rather than
//! quietly doing nothing.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use aui::data::button;
use aui::transcript::{
    activity_group, answered_row, approval_card, assistant_turn, error_card, generic_item_card,
    goal_card, handoff_card, live_activity_row, marker_row, parse_markdown, plan_card,
    question_card, runnable_command, summary_card, thinking_block, todo_list, tool_card,
    tool_group, turn_fold, user_turn, AssistantTurnAction, GenericItemIntent, HandoffIntent,
    LinkTarget, MarkdownBlock, MessageSelection, QuestionOutcome, SpanEvent, ToolCardAction,
    ToolCardIntent, ToolGroupData, ToolGroupIntent, TurnFoldIntent, UserTurnAction,
};
use aui_protocol::{
    ActivityState, Answer, ApprovalState, Block, Diff, DiffKind, DiffLine, HandoffState, Hunk, MarkerKind,
    PlanSection, PlanState, Step, ThinkingState, ToolBody, ToolCall, ToolKind, ToolStatus, Turn, TurnMeta,
};
use crate::handoff::{SummaryKind, handoff_card_started_ms, handoff_steps};
use crate::terminal::{RunRequest, send_enter_for_alt};
use aui_tokens::scale;
use aui_icons::IconName;
use aui_motion::stream_reveal;
use gpui::{div, prelude::*, px, relative, AnyElement, App, ElementId, SharedString, Window};
use gpui_kit::base::{h_flex, v_flex};

/// The title the pending approval card asks its question with.
///
/// Deliberately names Muse: the library's default is provider-free, because the
/// library does not know whose command it is and this app does.
pub const APPROVAL_TITLE: &str = "Allow Muse to run this command?";

/// The verb a provider approval title asks with, per tool: the card asks
/// what the tool would do, not what the last tool did.
fn approval_verb(tool: &str) -> String {
    match tool {
        "Bash" => "run this command".to_owned(),
        "Write" => "write this file".to_owned(),
        "Edit" | "MultiEdit" => "edit this file".to_owned(),
        "Read" => "read this file".to_owned(),
        "WebFetch" => "fetch this URL".to_owned(),
        "WebSearch" => "search the web".to_owned(),
        "Permissions" => "change permissions".to_owned(),
        tool if is_terminal_tool_name(tool) => "run this command in the terminal".to_owned(),
        other => format!("use {other}"),
    }
}

/// Whether `tool` names a baaz terminal tool on any lane: the muse lane's
/// bare `terminal_*` names and the Claude Code lane's `mcp__baaz__`
/// prefix. The approval title asks what the tool would do, so both
/// spellings map to the terminal verb.
fn is_terminal_tool_name(tool: &str) -> bool {
    const TOOLS: [&str; 7] = [
        "terminal_list",
        "terminal_open",
        "terminal_run",
        "terminal_read",
        "terminal_screen",
        "terminal_send",
        "terminal_close",
    ];
    TOOLS.iter().any(|name| tool == *name || tool == format!("mcp__baaz__{name}"))
}

/// The pending question for one approval card: the muse lane keeps
/// [`APPROVAL_TITLE`], and a provider session names its provider
/// ("Allow Claude Code to write this file?") with a verb that fits the
/// tool. `None` is the muse lane — the host is only ever `Some` beside
/// the fold's provider cards.
pub fn approval_title(host: Option<&str>, tool: &str) -> String {
    match host {
        Some(host) => format!("Allow {host} to {}?", approval_verb(tool)),
        None => APPROVAL_TITLE.to_owned(),
    }
}

/// What a collapsible card needs from the view: which cards the person has
/// toggled away from their default, and where a click on a header goes.
///
/// The set holds **overrides**, not open cards, so a card whose default changes
/// under it — a reasoning trace collapses the moment it finishes — still honours
/// a person who opened it by hand.
pub struct Folds {
    /// Keys (`"<turn id>:<block index>"`) of the cards toggled by hand.
    ///
    /// Shared, not copied: every one of these maps is read by the frame and
    /// written only by an intent, so a steady-state frame takes a refcount
    /// rather than rebuilding a collection per turn (finding `performance-3`).
    pub toggled: Rc<HashSet<String>>,
    /// Called with the key of the card whose header was clicked.
    pub toggle: ToggleHandler,
    /// Called when a plan card's action row is used. `None` renders the plan
    /// card read-only, which is what a replayed transcript wants.
    pub plan: Option<PlanHandler>,
    /// The live approval and question wiring. `None` renders both read-only.
    pub cards: Option<Cards>,
    /// Whose command a pending approval card asks about: `Some` ("Claude
    /// Code", "Codex") on a provider session, `None` on the muse lane
    /// (which keeps [`APPROVAL_TITLE`]). The card title names the host
    /// and a verb that fits the tool, per [`approval_title`].
    pub approval_host: Option<String>,
    /// Session id → the label the sidebar shows for it, so a `ForkedFrom`
    /// marker can name the session it came from rather than its uuid.
    pub titles: Rc<HashMap<String, String>>,
    /// Tool block id → what its truncated server-side output offers. Only
    /// blocks whose item carried `truncated: true` with an `outputRef` appear
    /// here; every other tool card keeps the plain fold toggle.
    pub full_output: Rc<HashMap<String, FullOutput>>,
    /// "Show full output" on a truncated tool card: the block's id, out. The
    /// card never fetches itself — the app pages `item/readOutput` on a
    /// background task and replaces the body on the server's result.
    pub show_full_output: Option<CardHandler>,
    /// Draw the cards settled rather than entering, for a `--screenshot` run
    /// that renders a few frames and quits.
    pub at_rest: bool,
    /// The clock a frame formats turn ages against, read once per frame
    /// ([`transcript_now_ms`]) so one frame formats once.
    pub now_ms: u64,
    /// Turn ids whose copy button shows the success check: the app sets the
    /// id on the copy intent and clears it after the library's `COPY_HOLD`
    /// (`aui::transcript::COPY_HOLD`), which the code-block header honours
    /// on its own. Empty almost always, so the steady-state frame clones
    /// nothing per turn.
    pub copied: Rc<HashSet<String>>,
    /// Link clicks from markdown bodies (C5): URLs and workspace paths.
    pub link: Option<TurnLinkHandler>,
    /// Bottom-row actions on assistant turns, keyed by turn id (C6).
    pub assistant_action: Option<AssistantActionHandler>,
    /// Bottom-row actions on user turns: turn id plus its text (C6).
    pub user_action: Option<UserActionHandler>,
    /// What each turn currently holds spanned, by turn id (C8b). Per turn
    /// because the library scopes cell keys to the markdown view that
    /// rendered them — one shared span would light up every turn at once.
    /// The markdown source is carried alongside so the map can be shared
    /// straight from the view rather than re-collected each frame. Keyed,
    /// not positional, so a span survives the transcript scrolling mid-drag.
    pub span_held: Rc<HashMap<String, (String, MessageSelection)>>,
    /// Span events out of the turns: turn id, that turn's markdown source,
    /// and the event, for the turn's drag session (C8b).
    pub span_event: Option<SpanEventHandler>,
    /// Tool-group header and per-call intents, keyed by the group's fold key (C8).
    pub tool_group: Option<ToolGroupActionHandler>,
    /// D49 play buttons: run the request in the terminal dock. Wired on
    /// shell tool cards ("Run in terminal") and under assistant turns with
    /// runnable fences ("Run"), and honoured under `--replay` — the request
    /// is entirely local, never a turn, never the wire.
    pub terminal_run: Option<TerminalRunHandler>,
    /// D51 "Open terminal" on terminal tool cards: the tab to focus, when
    /// the card names one — `None` opens the dock on its active tab.
    /// Entirely local like [`Folds::terminal_run`], honoured under
    /// `--replay` too.
    pub open_terminal: Option<OpenTerminalHandler>,
    /// D51 live mirror: tool-call id → the block's last lines and whether
    /// it is still running, snapshotted once per frame by the session view
    /// from the terminal host. Empty under `--replay` (no host), where
    /// the card renders the folded result text instead.
    pub terminal_mirror: Rc<HashMap<String, TerminalMirror>>,
    /// Whether any terminal card's tab is still busy this frame: the
    /// session view holds one animation frame while true, so the mirror
    /// tracks arriving output — one repaint per frame, none once settled.
    pub terminal_live: bool,
    /// Skill loads, by skill name: the scope the landed catalog reports, so
    /// the quiet row reads "Loaded skill `name` · scope" (D63). Empty when
    /// the catalog knows no such skill — the scope is omitted, never
    /// guessed.
    pub skill_scopes: Rc<HashMap<String, String>>,
    /// A skill row's tap: the skill name, out to the Skills page. `None`
    /// renders the row read-only.
    pub open_skill: Option<SkillOpenHandler>,
    /// The live handoff wiring: the card's two intents. `None` renders the
    /// card read-only.
    pub handoff: Option<HandoffCards>,
    /// The destination marker's back-link to the source session. `None`
    /// renders the marker without its link.
    pub handoff_back: Option<HandoffBack>,
    /// The session's workspace root, for shortening file-card paths
    /// through [`display_path`]. Display only: the blocks keep their full
    /// targets, so clicks, reveals and run-in-terminal are unchanged.
    pub workspace_root: String,
    /// B12: whether settled assistant turns fold their work behind one
    /// turn-fold row (default on). Off leaves every run expanded after
    /// settle, for people auditing agents live. The persisted Settings
    /// switch lives outside this module's scope; this is the per-frame
    /// backing the view fills in.
    pub fold_finished_turns: bool,
    /// B12: "Open full text" on a generic card (and the open-in-pane fold
    /// row on search/MCP cards): the card's title and its whole text, out
    /// to the right pane's read-only doc view. `None` leaves the card's
    /// own capped body as the whole affordance.
    pub open_full_text: Option<FullTextHandler>,
    /// B12: the turn fold's `+N −M` chip: the turn id, out to the Changes
    /// view scoped to that turn (or the session without a per-turn
    /// checkpoint). `None` draws the chip without its tap.
    pub open_turn_diff: Option<CardHandler>,
    /// Q1b: tool-call id → the wall time (epoch ms) its block first
    /// appeared, snapshotted once per frame by the session view. Live
    /// run rows tick from the run's first member start, never from summed
    /// durations; unknown ids fall back to the turn timestamp.
    pub tool_starts: Rc<HashMap<String, u64>>,
}

/// Q1b: stamp first-seen wall times for every tool-call id in `turns`
/// (group members included) and drop ids that left the transcript, so the
/// map stays bounded by what is on screen. The session view calls this
/// where applied deltas materialise, with the frame's own clock.
pub fn refresh_tool_starts(turns: &[Rc<Turn>], starts: &mut HashMap<String, u64>, now_ms: u64) {
    let mut seen = HashSet::new();
    for turn in turns {
        let Turn::Assistant { blocks, .. } = turn.as_ref() else { continue };
        for block in blocks {
            match block {
                Block::ToolCall { id, .. } => {
                    seen.insert(id.clone());
                    starts.entry(id.clone()).or_insert(now_ms);
                }
                Block::ToolGroup { calls, .. } => {
                    for call in calls {
                        seen.insert(call.id.clone());
                        starts.entry(call.id.clone()).or_insert(now_ms);
                    }
                }
                _ => {}
            }
        }
    }
    starts.retain(|id, _| seen.contains(id));
}

/// B12: show a whole text elsewhere (the right pane's doc view): the
/// card's title and its full text, out.
pub type FullTextHandler = Rc<dyn Fn(String, String, &mut Window, &mut App)>;

/// What a handoff card needs to talk back: "Open the new session" opens
/// the destination by id, "Cancel" aborts the move by the card's block id.
pub struct HandoffCards {
    /// Open a session by id: the card's destination session.
    pub open: CardHandler,
    /// Cancel the move: the card's own block id.
    pub cancel: CardHandler,
}

/// The destination marker's way back to the source session.
pub struct HandoffBack {
    /// The source session the link opens.
    pub source: String,
    /// Open it.
    pub open: CardHandler,
}

/// Whether a tool card is a skill load: a Read card whose verb is the
/// "Loaded skill" both folds mint (D63). The name is the card's target.
pub fn skill_load_name(call: &ToolCall) -> Option<&str> {
    match (&call.kind, call.verb.as_str(), call.target.trim()) {
        (ToolKind::Read, "Loaded skill", target) if !target.is_empty() => Some(target),
        _ => None,
    }
}

/// The quiet row's text: "Loaded skill `name`", with the scope joined when
/// the catalog reported one (D63).
pub fn skill_load_text(name: &str, scope: Option<&str>) -> String {
    match scope.filter(|scope| !scope.trim().is_empty()) {
        Some(scope) => format!("Loaded skill `{name}` · {scope}"),
        None => format!("Loaded skill `{name}`"),
    }
}

/// A play button press: the resolved command and whether Enter follows the
/// paste. The view emits it as [`crate::session::SessionEvent::RunInTerminal`];
/// the application owns the dock.
pub type TerminalRunHandler = Rc<dyn Fn(RunRequest, &mut Window, &mut App)>;

/// An "Open terminal" press: the tab to focus, when the card names one.
/// The view emits it as [`crate::session::SessionEvent::OpenTerminal`];
/// the application opens the dock on that tab.
pub type OpenTerminalHandler = Rc<dyn Fn(Option<String>, &mut Window, &mut App)>;

/// One frame's live mirror of a terminal tab's block (D51): the block's
/// last lines for the card body, and whether the tab is still busy.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalMirror {
    /// The block's last lines, oldest first — at most
    /// [`TERMINAL_MIRROR_LINES`].
    pub lines: Vec<String>,
    /// The tab still holds a running block: the card keeps its live pill
    /// and the view keeps repainting, one frame at a time.
    pub live: bool,
}

/// How many of the block's last lines the terminal card mirrors (D51).
pub const TERMINAL_MIRROR_LINES: usize = 6;

/// A skill row's tap: the skill name, out. The view emits it as
/// [`crate::session::SessionEvent::OpenSkill`]; the application opens the
/// Skills page on that skill.
pub type SkillOpenHandler = Rc<dyn Fn(String, &mut Window, &mut App)>;

/// The header action id of "Run in terminal" on shell tool cards.
pub const RUN_IN_TERMINAL_ACTION_ID: &str = "run-in-terminal";

/// The header action label of "Run in terminal" on shell tool cards.
pub const RUN_IN_TERMINAL_LABEL: &str = "Run in terminal";

/// The header action id of "Open terminal" on terminal tool cards (D51):
/// opens the dock and focuses the card's tab.
pub const OPEN_IN_TERMINAL_ACTION_ID: &str = "open-terminal";

/// The header action label of "Open terminal" on terminal tool cards.
/// The label is the accessible name: the library's button only takes a
/// role with an explicit accessibility label, and its action slot carries
/// id/label/icon alone — so the human label here is what a screen reader
/// has to announce the control with.
pub const OPEN_IN_TERMINAL_LABEL: &str = "Open terminal";

/// Whether a tool call is a terminal tool call (D51): a shell card whose
/// verb is the folds' terminal verb, on every lane. Only these cards get
/// the "Open terminal" action and the live mirror; every other shell card
/// keeps "Run in terminal".
pub fn is_terminal_card(call: &ToolCall) -> bool {
    matches!(&call.kind, ToolKind::Shell)
        && (call.verb == TERMINAL_RUNNING_VERB || call.verb == TERMINAL_RAN_VERB)
}

/// The opening verb every lane's fold gives a terminal card.
pub const TERMINAL_RUNNING_VERB: &str = "Running in terminal";

/// The settled verb every lane's fold gives a terminal card.
pub const TERMINAL_RAN_VERB: &str = "Ran in terminal";

/// The tab a terminal target names (`"<command> · t1"`), when the result
/// named one. The suffix is the only channel the frozen tool-call shape
/// leaves the fold, so the parse is strict — a trailing `· t<digits>` —
/// and anything else is no tab rather than a wrong one.
pub fn terminal_tab_id(target: &str) -> Option<String> {
    let (_, tab) = target.rsplit_once('·')?;
    let tab = tab.trim();
    if tab.len() > 1 && tab.starts_with('t') && tab[1..].chars().all(|c| c.is_ascii_digit()) {
        Some(tab.to_owned())
    } else {
        None
    }
}

/// The command a terminal target ran: the target short of the tab suffix,
/// for matching the tab's block to mirror.
pub fn terminal_command(target: &str) -> String {
    match target.rsplit_once('·') {
        Some((command, tab)) if terminal_tab_id(target).is_some_and(|id| id == tab.trim()) => {
            command.trim_end().to_owned()
        }
        _ => target.to_owned(),
    }
}

/// The run row's button label under assistant turns with runnable fences.
pub const RUN_LABEL: &str = "Run";

/// The command a shell tool card offers to run (D49): Muse's shell tool and
/// the `!` userShell both fold to [`ToolKind::Shell`] with a
/// [`ToolBody::Shell`] body, so one match covers both. The card's command
/// text is its target; anything else — a read, a search that fell back to a
/// shell body, a blank target — offers no button. Terminal cards offer
/// none either: their command already ran in the dock, and their action
/// is "Open terminal", not a rerun.
pub fn shell_run_command(call: &ToolCall) -> Option<String> {
    if is_terminal_card(call) {
        return None;
    }
    match (&call.kind, &call.body) {
        (ToolKind::Shell, ToolBody::Shell { .. }) if !call.target.trim().is_empty() => {
            Some(call.target.clone())
        }
        _ => None,
    }
}

/// X2: a Write/Edit card whose diff changes more than this many lines
/// starts closed (header + diffstat visible, body hidden); smaller edits
/// keep the open default. Changed lines are added + removed rows —
/// context rows are not changes.
pub const BIG_EDIT_CHANGED_LINES: usize = 12;

/// X2: an opened big diff draws at most this many rows of its first hunk,
/// then a final `… N more lines` row. The stored block keeps the whole
/// diff; only the copy handed to the card is cut.
pub const DIFF_DISPLAY_CAP: usize = 40;

/// X2: a shell card's header names at most this many characters of the
/// command's first line; the block keeps the whole command.
pub const COMMAND_TARGET_CHARS: usize = 160;

/// X2: whether a lone tool card starts open. A Write/Edit card with more
/// than [`BIG_EDIT_CHANGED_LINES`] changed lines starts closed — a
/// 233-line file write is a wall of text nobody asked to re-read — while
/// small edits and every other card keep today's open default. The toggle
/// still persists through `folds` either way, so a person can open what
/// starts closed.
pub fn default_open(call: &ToolCall) -> bool {
    match &call.body {
        ToolBody::Edit { diff } => changed_lines(diff) <= BIG_EDIT_CHANGED_LINES,
        _ => true,
    }
}

/// The changed rows of a diff: every non-context row of every hunk.
fn changed_lines(diff: &Diff) -> usize {
    diff.hunks
        .iter()
        .flat_map(|hunk| hunk.lines.iter())
        .filter(|line| !matches!(line.kind, DiffKind::Context))
        .count()
}

/// X2: the diff a card actually draws: the first `cap` rows of the first
/// hunk, plus a final muted `… N more lines` context row when rows were
/// cut (`N` counts the first hunk's dropped rows; later hunks are kept).
/// Short diffs come back unchanged. The input is never mutated — the
/// caller draws this copy and the stored block keeps the full diff. aui
/// v0.3.1 offers no line-level "show all" affordance (its fold row only
/// covers further hunks), so the truncation plus the count is the whole
/// affordance for now.
pub fn display_diff(diff: &Diff, cap: usize) -> Diff {
    // Only an over-long FIRST hunk is cut; every later hunk stays, so the
    // library's own fold row still reaches them. Cutting at the first hunk
    // regardless dropped hunks 2..N of every multi-hunk edit (X2 review).
    let Some(first) = diff.hunks.first() else { return diff.clone() };
    if first.lines.len() <= cap {
        return diff.clone();
    }
    let omitted = first.lines.len() - cap;
    let mut lines: Vec<DiffLine> = first.lines.iter().take(cap).cloned().collect();
    lines.push(DiffLine {
        kind: DiffKind::Context,
        old_no: None,
        new_no: None,
        text: format!("\u{2026} {omitted} more lines"),
    });
    let mut hunks = vec![Hunk { header: first.header.clone(), lines }];
    hunks.extend(diff.hunks.iter().skip(1).cloned());
    Diff {
        path: diff.path.clone(),
        hunks,
        added: diff.added,
        removed: diff.removed,
    }
}

/// B8b: the files the session edited, in edit order: every Edit/Write tool
/// block's target across the folded turns, lone calls and grouped calls
/// alike. Every lane folds its edit-family tools — `apply_patch` included
/// — to [`ToolKind::Edit`]/[`ToolKind::Write`], so the kind is the whole
/// filter; reads, searches, shell commands and skill rows never join.
/// Blank targets are skipped and repeats keep their first position, so the
/// Changes pane's "This session" section lists each file once, in the order
/// the session touched it.
/// B8c: the [`session_edited_paths`] cache key the Changes pane holds on the
/// session view: (turn count, last turn's block count). O(1) — no block scan —
/// so the shell render skips the O(all blocks) walk on frames that do not
/// show Changes, and on steady frames where the key stands still.
pub fn edited_paths_cache_key(turns: &[Turn]) -> (usize, usize) {
    let last_blocks = turns
        .last()
        .map(|turn| match turn {
            Turn::Assistant { blocks, .. } => blocks.len(),
            Turn::User { .. } => 0,
        })
        .unwrap_or(0);
    (turns.len(), last_blocks)
}

pub fn session_edited_paths(turns: &[Turn]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut ordered = Vec::new();
    let mut push = |kind: &ToolKind, target: &str| {
        if !matches!(kind, ToolKind::Edit | ToolKind::Write) {
            return;
        }
        let path = target.trim();
        if path.is_empty() || !seen.insert(path.to_owned()) {
            return;
        }
        ordered.push(path.to_owned());
    };
    for turn in turns {
        let Turn::Assistant { blocks, .. } = turn else { continue };
        for block in blocks {
            match block {
                Block::ToolCall { kind, target, .. } => push(kind, target),
                Block::ToolGroup { calls, .. } => {
                    for call in calls {
                        push(&call.kind, &call.target);
                    }
                }
                _ => {}
            }
        }
    }
    ordered
}

/// Z6b: a file-card path for display. Inside the session's workspace
/// root it reads relative to the root (`crates/baaz/src/app.rs`); outside
/// it stays absolute but abbreviates `$HOME` to `~`. A sibling directory
/// sharing a string prefix (`/a/harness-int` vs root `/a/harness`) is
/// outside: the check is component-wise, through [`std::path::Path`].
/// Anything else — relative targets, patterns, blank — passes through
/// untouched. Display only: clicks and reveals keep the full path.
pub fn display_path(path: &str, workspace_root: &str, home: &str) -> String {
    let candidate = std::path::Path::new(path);
    if candidate.is_absolute() {
        let root = std::path::Path::new(workspace_root);
        // An empty root (a session with no workspace) or `/` contains every
        // absolute path; neither is a workspace to be relative to.
        let has_root = root.is_absolute() && root.parent().is_some();
        if let Some(relative) = has_root.then(|| candidate.strip_prefix(root).ok()).flatten() {
            if !relative.as_os_str().is_empty() {
                return relative.to_string_lossy().into_owned();
            }
            // The root itself: there is no relative remainder to show.
            return path.to_owned();
        }
        if !home.is_empty() {
            // `Path::strip_prefix` consumes the separator, so the
            // remainder is already relative (`other/file.rs`).
            if let Ok(rest) = candidate.strip_prefix(home) {
                if rest.as_os_str().is_empty() {
                    return "~".to_owned();
                }
                return format!("~/{}", rest.to_string_lossy());
            }
        }
    }
    path.to_owned()
}

/// The process `$HOME`, or empty when unset — the [`display_path`]
/// abbreviation then simply never fires.
fn home_dir() -> String {
    std::env::var_os("HOME").map(|home| home.to_string_lossy().into_owned()).unwrap_or_default()
}

/// X2: the header target a shell card shows: the command's first line
/// only, capped at [`COMMAND_TARGET_CHARS`] characters with `…`. A shell
/// heredoc otherwise makes the whole script the card's title. The block
/// keeps the full command — run-in-terminal still runs it whole. Z6b: a
/// non-shell card's target is a file path, shown through [`display_path`]
/// against the session workspace — the block keeps the full target, so
/// reveals and the terminal tab lookup still see the whole path. Covers
/// every lane: Codex, Claude Code and muse shell cards all render through
/// `tool_call_card`.
pub fn display_target(call: &ToolCall, workspace_root: &str, home: &str) -> String {
    if !matches!(call.kind, ToolKind::Shell) {
        return display_path(&call.target, workspace_root, home);
    }
    let first = call.target.lines().next().unwrap_or("").trim_end().to_owned();
    if first.chars().count() > COMMAND_TARGET_CHARS {
        let mut short: String = first.chars().take(COMMAND_TARGET_CHARS).collect();
        short.push('…');
        short
    } else {
        first
    }
}

/// Whether the pressed header action is the open-terminal action: the
/// payload of [`ToolCardIntent::Action`] is the action's index in the
/// order the card received them, so the check is against the id at that
/// index — the same index rule [`action_is_run`] follows.
pub fn action_is_open(actions: &[ToolCardAction], index: usize) -> bool {
    actions.get(index).is_some_and(|action| action.id == OPEN_IN_TERMINAL_ACTION_ID)
}

/// Whether the pressed header action is the run action: the payload of
/// [`ToolCardIntent::Action`] is the action's index in the order the card
/// received them, so the check is against the id at that index — a card
/// carrying more than one action still maps back to the right command.
pub fn action_is_run(actions: &[ToolCardAction], index: usize) -> bool {
    actions.get(index).is_some_and(|action| action.id == RUN_IN_TERMINAL_ACTION_ID)
}

/// The command a fenced code block offers to run (D49): exactly what
/// [`runnable_command`] returns — the decision function owns which fences
/// count, so a `console` block runs its prompt lines with prompts stripped,
/// and a `rust` block offers no button.
pub fn code_run_command(lang: Option<&str>, code: &str) -> Option<String> {
    runnable_command(lang.unwrap_or(""), code)
}

/// One runnable fence of an assistant turn, in fence order: the label the
/// run row shows and the exact command the press runs.
pub struct RunnableBlock {
    /// The command's first line, elided: what the run row names.
    pub label: String,
    /// What [`runnable_command`] returned: what the press runs.
    pub command: String,
}

/// A command's first line as a one-line label, overlong commands cut with
/// an ellipsis.
fn run_label(command: &str) -> String {
    const MAX: usize = 80;
    let first = command.lines().next().unwrap_or("").trim();
    if first.chars().count() <= MAX {
        return first.to_owned();
    }
    let cut: String = first.chars().take(MAX - 1).collect();
    format!("{cut}…")
}

/// The runnable fenced blocks of assistant prose, in fence order: exactly
/// the fences [`runnable_command`] returns `Some` for, running what it
/// returns.
pub fn runnable_blocks(text: &str) -> Vec<RunnableBlock> {
    parse_markdown(text)
        .iter()
        .filter_map(|block| match block {
            MarkdownBlock::CodeBlock { lang, text } => {
                code_run_command(lang.as_deref(), text).map(|command| RunnableBlock {
                    label: run_label(&command),
                    command,
                })
            }
            _ => None,
        })
        .collect()
}

/// A markdown link click: the target the library parsed out.
pub type TurnLinkHandler = Rc<dyn Fn(LinkTarget, &mut Window, &mut App)>;

/// An assistant turn's bottom-row action: the turn's id and what was pressed.
pub type AssistantActionHandler = Rc<dyn Fn(String, AssistantTurnAction, &mut Window, &mut App)>;

/// A user turn's bottom-row action: the turn's id, its text, and what was pressed.
pub type UserActionHandler = Rc<dyn Fn(String, String, UserTurnAction, &mut Window, &mut App)>;

/// A turn's span event: the turn's id, that turn's markdown source (so ⌘C
/// slices the exact view the person dragged in), and the event itself —
/// presses, hovers, releases and word/paragraph picks (C8b).
pub type SpanEventHandler = Rc<dyn Fn(String, String, SpanEvent, &mut Window, &mut App)>;

/// A tool group's intent: the group's fold key and what it asked for.
pub type ToolGroupActionHandler = Rc<dyn Fn(String, ToolGroupIntent, &mut Window, &mut App)>;

/// What a card header click reports: the key of the card that was clicked.
pub type ToggleHandler = Rc<dyn Fn(String, &mut Window, &mut App)>;

/// What the person chose on a plan card (spec §3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanAction {
    /// Restore the previous approval mode and implement the plan.
    Accept,
    /// Keep plan mode and go back to the composer.
    Refine,
    /// Restore the previous approval mode and do nothing else.
    Reject,
}

/// Called with the plan block's id and the action taken.
pub type PlanHandler = Rc<dyn Fn(String, PlanAction, &mut Window, &mut App)>;

/// An intent that names one card and nothing else: the block's id, out.
///
/// "Continue", "Skip", "Explain instead" and "Retry" all have this shape — the
/// card is the whole argument, because which card it was is the only thing the
/// app needs in order to know what to send.
pub type CardHandler = Rc<dyn Fn(String, &mut Window, &mut App)>;

/// An intent that names a card and a row inside it: `(block id, index)`.
///
/// An option was clicked; an option's preview was toggled.
pub type RowHandler = Rc<dyn Fn(String, usize, &mut Window, &mut App)>;

/// What a tool card offers when the server truncated its visible output but
/// kept the full bytes under an `outputRef`.
#[derive(Clone)]
pub struct FullOutput {
    /// The fold holds the item's `outputRef`, so a fetch would serve bytes.
    pub fetchable: bool,
    /// The fetch the app runs, and what it returned.
    pub state: FullOutputState,
}

/// Where a truncated tool card's full-output fetch stands.
#[derive(Clone)]
pub enum FullOutputState {
    /// Nothing fetched yet; the fold row fetches.
    Idle,
    /// Pages are arriving on the app's background task.
    Fetching,
    /// The server's bytes, as lines, with whether the 2 MiB cap cut them.
    Ready {
        lines: Vec<String>,
        capped: bool,
    },
}

/// A server-minted approval choice was pressed:
/// `(approvalId, choiceId, feedback)`.
pub type ChooseHandler = Rc<dyn Fn(String, String, Option<String>, &mut Window, &mut App)>;

/// Open (`Some(choiceId)`) or close (`None`) an approval's feedback field, by
/// `approvalId`.
pub type FeedbackToggleHandler = Rc<dyn Fn(String, Option<String>, &mut Window, &mut App)>;

/// Everything a pending approval or question card needs to be answerable.
///
/// It is one struct rather than a dozen parameters because the two cards share
/// the same shape: some state the app owns (what is selected, which field is
/// open), one element the app owns (the text field), and a handful of intents
/// out. The two `RefCell<Option<AnyElement>>` slots are the fields themselves:
/// only one card can have one open at a time, so the first card that matches
/// takes it.
pub struct Cards {
    /// A server-minted choice was pressed: `(approvalId, choiceId, feedback)`.
    pub choose: ChooseHandler,
    /// Open (`Some(choiceId)`) or close (`None`) an approval's feedback field.
    pub feedback_toggle: FeedbackToggleHandler,
    /// Which approval has a feedback field open, and for which choice.
    pub feedback_open: Option<(String, String)>,
    /// That field. Taken by the card that owns it.
    pub feedback_slot: RefCell<Option<AnyElement>>,
    /// What the field currently holds, so "Send" can carry it.
    pub feedback_text: String,
    /// An option row was clicked: `(question block id, option index)`.
    pub select: RowHandler,
    /// What is selected on each pending question, by block id.
    pub selections: HashMap<String, Vec<usize>>,
    /// An option's preview chevron: `(question block id, option index)`.
    pub toggle_preview: RowHandler,
    /// Which previews are open, by block id.
    pub previews: HashMap<String, Vec<usize>>,
    /// "Continue" on a question: its block id.
    pub answer: CardHandler,
    /// "Skip" on a question: its block id.
    pub skip: CardHandler,
    /// "Explain instead": the question's block id, or the confirmation of an
    /// already-open field.
    pub clarify: CardHandler,
    /// Which question has its clarification field open.
    pub clarify_open: Option<String>,
    /// That field.
    pub clarify_slot: RefCell<Option<AnyElement>>,
    /// How long each pending question has left, by block id, against how long
    /// it was given. The app ticks the clock; the card only draws it.
    pub countdowns: HashMap<String, (u64, u64)>,
    /// "Retry" on an error card: the id of the turn that failed.
    pub retry: CardHandler,
    /// Which failed turns still have their prompt text, so the button is only
    /// offered where pressing it would do something.
    pub retryable_turns: HashSet<String>,
}

impl Folds {
    /// Whether the card at `key` is open, given what it does by default.
    fn open(&self, key: &str, default_open: bool) -> bool {
        default_open != self.toggled.contains(key)
    }
}

/// The key that identifies one block for folding and for its element id.
pub fn block_key(turn_id: &str, index: usize) -> String {
    format!("{turn_id}:{index}")
}

/// B12: the key that identifies one settled turn's fold row: the header
/// toggle and the row mapping share it, so expanding and row counts agree.
pub fn fold_key(turn_id: &str) -> String {
    format!("{turn_id}:fold")
}

/// B12: the key that identifies one live run's row: the run starting at
/// block `start` of `turn_id`.
pub fn live_key(turn_id: &str, start: usize) -> String {
    format!("{turn_id}:live:{start}")
}

/// B12: what a settled assistant turn folds to: one header row summarising
/// the work, the final answer outside it, and the kept-visible kinds
/// (denied approvals, unrecovered failures, answered questions,
/// plans/todos, handoff dividers/cards) staying out with the answer.
/// B12fix: cached per turn (see [`fold_plan_cached`]), so it is `Clone`.
#[derive(Clone, Debug)]
pub struct FoldPlan {
    /// Files read (read-family tool calls, group members included).
    pub reads: u32,
    /// Files edited or written.
    pub edits: u32,
    /// Shell commands run.
    pub commands: u32,
    /// The turn's wall-clock time, from its meta.
    pub elapsed_ms: u64,
    /// Summed `+` lines over the turn's diff stats.
    pub diff_added: u64,
    /// Summed `−` lines over the turn's diff stats.
    pub diff_removed: u64,
    /// Block indices hidden while the fold is closed, in turn order:
    /// tool cards, groups, thinking, and interim prose.
    pub folded: Vec<usize>,
    /// Block indices that stay visible while closed, in turn order.
    pub visible: Vec<usize>,
    /// The final answer's block index: the last non-blank text block.
    /// Rendered after the fold, never inside it.
    pub answer: Option<usize>,
}

/// B12: whether an approval card stays visible after settle: only a
/// granted approval folds away. Pending, approving (stuck), denied and
/// refused approvals shaped the answer, so they stay out.
fn approval_folds(state: &ApprovalState) -> bool {
    matches!(state, ApprovalState::AllowedOnce { .. } | ApprovalState::AutoAllowed { .. })
}

/// B12: count one tool call's family for the fold summary.
fn count_call(plan: &mut FoldPlan, call: &ToolCall) {
    match &call.kind {
        ToolKind::Read => plan.reads += 1,
        ToolKind::Edit | ToolKind::Write => plan.edits += 1,
        ToolKind::Shell => plan.commands += 1,
        _ => {}
    }
    if let Some(stat) = &call.diff_stat {
        plan.diff_added += stat.added;
        plan.diff_removed += stat.removed;
    }
}

/// B12: whether a tool call keeps its own row even inside a settled turn:
/// an error or cancelled call is an unrecovered failure until proven
/// otherwise, so it stays visible.
fn call_failed(call: &ToolCall) -> bool {
    matches!(call.status, ToolStatus::Error | ToolStatus::Cancelled)
}

/// B12fix: an empty Thinking block takes no row at all — live or
/// settled, folded or open. The folds already refuse to mint one (a
/// redacted trace is signature without utterance); this is the
/// transcript side of the same rule, for blocks built by hand.
pub fn is_empty_thinking(block: &Block) -> bool {
    matches!(block, Block::Thinking { text, .. } if text.trim().is_empty())
}

/// B12fix: what identifies a tool call for recovery: the tool's family
/// and what it worked on. A failed call followed later in the turn by a
/// successful call with the same identity recovered — the retry is the
/// visible outcome, so the failure folds.
fn tool_identity(call: &ToolCall) -> (std::mem::Discriminant<ToolKind>, &str, &str) {
    (std::mem::discriminant(&call.kind), call.verb.as_str(), call.target.as_str())
}

/// B12fix: whether a failed top-level tool call recovered later in the
/// turn: a successful call with the same tool identity sits past it.
/// Recovered failures fold; unrecovered ones stay visible.
fn recovered_failure(blocks: &[Block], index: usize, call: &ToolCall) -> bool {
    let identity = tool_identity(call);
    blocks.iter().skip(index + 1).filter_map(|block| block.as_tool_call()).any(|later| {
        later.status == ToolStatus::Success && tool_identity(&later) == identity
    })
}

/// B12: the fold plan for one assistant turn, or `None` when the turn gets
/// no fold row: user turns, and assistant turns with no tool activity
/// (a fold must hide something).
///
/// B12fix answer rule: the answer is the trailing Text block(s) after the
/// last non-Text block. A turn ending on prose answers with its last
/// non-blank trailing text (earlier trailing texts stay out with it); a
/// turn ending on a tool or an error has no separate answer — the last
/// non-blank Text stays visible in its original position along with the
/// ending failure. The old rule (last non-empty Text anywhere) put the
/// answer before kept-visible blocks that came after it.
pub fn fold_plan(turn: &Turn) -> Option<FoldPlan> {
    let Turn::Assistant { blocks, meta, .. } = turn else { return None };
    if !blocks.iter().any(|block| {
        matches!(block, Block::ToolCall { .. } | Block::ToolGroup { .. })
    }) {
        return None;
    }
    let mut plan = FoldPlan {
        reads: 0,
        edits: 0,
        commands: 0,
        elapsed_ms: meta.duration_ms,
        diff_added: 0,
        diff_removed: 0,
        folded: Vec::new(),
        visible: Vec::new(),
        answer: None,
    };
    // Where the prose tail starts: past the last non-Text block. `None`
    // is an all-text turn, which returns `None` above (no tool activity),
    // so the tail always has a head here.
    let last_non_text = blocks.iter().rposition(|block| !matches!(block, Block::Text { .. }));
    let ends_on_text = matches!(blocks.last(), Some(Block::Text { .. }));
    // The last non-blank Text anywhere: the answer candidate when the
    // turn ends on prose, the kept-visible prose when it ends on a tool.
    let last_text = blocks.iter().enumerate().rev().find_map(|(index, block)| match block {
        Block::Text { text, .. } if !text.trim().is_empty() => Some(index),
        _ => None,
    });
    if ends_on_text {
        // The final answer: the last text block with anything to say. An
        // all-blank tail is not an answer — it folds away with the work.
        plan.answer = last_text;
    }
    for (index, block) in blocks.iter().enumerate() {
        if Some(index) == plan.answer {
            continue;
        }
        // An empty Thinking block takes no row: excluded from both lists,
        // so neither the closed mapping nor the open one addresses it.
        if is_empty_thinking(block) {
            continue;
        }
        match block {
            Block::ToolCall { .. } => {
                let call = block.as_tool_call().expect("matched ToolCall");
                count_call(&mut plan, &call);
                if call_failed(&call) && !recovered_failure(blocks, index, &call) {
                    plan.visible.push(index);
                } else {
                    plan.folded.push(index);
                }
            }
            Block::ToolGroup { calls, .. } => {
                for call in calls {
                    count_call(&mut plan, call);
                }
                // A group with a failed call stays visible (collapsed, so
                // its header and preview name the failure): the fold row
                // counts reads/edits/commands only and would hide it.
                if calls.iter().any(call_failed) {
                    plan.visible.push(index);
                } else {
                    plan.folded.push(index);
                }
            }
            Block::Thinking { .. } | Block::Generic { .. } => {
                plan.folded.push(index);
            }
            Block::Text { text, .. } => {
                if text.trim().is_empty() {
                    plan.folded.push(index);
                } else if ends_on_text {
                    // Trailing prose stays out with the answer; interim
                    // prose (before the tail's head) folds.
                    let in_tail = last_non_text.is_none_or(|head| index > head);
                    if in_tail {
                        plan.visible.push(index);
                    } else {
                        plan.folded.push(index);
                    }
                } else {
                    // No answer: only the last Text stays visible, in its
                    // original position with the ending failure; earlier
                    // prose folds.
                    if Some(index) == last_text {
                        plan.visible.push(index);
                    } else {
                        plan.folded.push(index);
                    }
                }
            }
            Block::Approval { state, .. } => {
                if approval_folds(state) {
                    plan.folded.push(index);
                } else {
                    plan.visible.push(index);
                }
            }
            // Kept visible after settle: failures, answered questions,
            // plans/todos, handoff dividers/cards, activity and summaries.
            Block::Error { .. }
            | Block::Question { .. }
            | Block::Plan { .. }
            | Block::Todo { .. }
            | Block::Marker { .. }
            | Block::Handoff { .. }
            | Block::Activity { .. }
            | Block::Summary { .. }
            | Block::Goal { .. } => plan.visible.push(index),
        }
    }
    Some(plan)
}

/// B12fix: a cheap fingerprint of a turn's blocks, so the cached fold
/// plan notices in-place updates (a running call settling, a failure
/// landing) that keep the block count: kind per index, failure state,
/// and text length. Part of the cache key beside the specified four.
fn blocks_fingerprint(blocks: &[Block]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for block in blocks {
        match block {
            Block::ToolCall { .. } => {
                let call = block.as_tool_call().expect("matched ToolCall");
                0u8.hash(&mut hasher);
                tool_identity(&call).hash(&mut hasher);
                (call_failed(&call) as u8).hash(&mut hasher);
            }
            Block::ToolGroup { calls, .. } => {
                1u8.hash(&mut hasher);
                calls.len().hash(&mut hasher);
                calls.iter().filter(|call| call_failed(call)).count().hash(&mut hasher);
            }
            Block::Thinking { text, .. } => {
                2u8.hash(&mut hasher);
                text.len().hash(&mut hasher);
            }
            Block::Text { text, .. } => {
                3u8.hash(&mut hasher);
                text.len().hash(&mut hasher);
            }
            Block::Approval { state, .. } => {
                4u8.hash(&mut hasher);
                approval_folds(state).hash(&mut hasher);
            }
            other => {
                5u8.hash(&mut hasher);
                std::mem::discriminant(other).hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

/// B12fix: the cached fold plan for one assistant turn: `None` for turns
/// with no plan, exactly like [`fold_plan`]. Keyed by turn id, block
/// count, busyness (settled or live), the fold's toggled state, and a
/// fingerprint of the blocks — so steady-state rebuilds skip the walk
/// while an in-place update (a settle, a failure) still re-plans. Bounded:
/// past the cap the whole cache drops rather than growing with the
/// transcript.
/// B12fix: what the per-turn fold cache holds, keyed by turn id, block
/// count, busyness, toggled state and block fingerprint.
type FoldCache = HashMap<(String, usize, bool, bool, u64), Option<FoldPlan>>;

pub fn fold_plan_cached(turn: &Turn, settled: bool, toggled: &HashSet<String>) -> Option<FoldPlan> {
    use std::cell::RefCell;
    use std::collections::HashMap;
    thread_local! {
        static FOLD_CACHE: RefCell<FoldCache> = RefCell::new(HashMap::new());
    }
    const FOLD_CACHE_CAP: usize = 128;
    let Turn::Assistant { id, blocks, .. } = turn else { return None };
    let key = (
        id.clone(),
        blocks.len(),
        settled,
        toggled.contains(&fold_key(id)),
        blocks_fingerprint(blocks),
    );
    if let Some(hit) = FOLD_CACHE.with(|cache| cache.borrow().get(&key).cloned()) {
        return hit;
    }
    let plan = fold_plan(turn);
    FOLD_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() >= FOLD_CACHE_CAP {
            cache.clear();
        }
        cache.insert(key, plan.clone());
    });
    plan
}

/// B12: one live run of tool calls: consecutive quiet calls with no prose
/// between them, drawn as one [`live_activity_row`] naming the current
/// (last) call.
pub struct LiveRun {
    /// The run's block indices, in turn order; the current call is last.
    pub blocks: Vec<usize>,
    /// Verb naming the current call: present tense while in flight
    /// ("Running"), past tense once everything finished ("Ran").
    pub verb: String,
    /// What the current call works on.
    pub target: String,
    /// Calls before the current one in the run.
    pub earlier: usize,
    /// The run's wall-clock time, saturating at the frame's clock.
    pub elapsed_ms: u64,
    /// Whether any member call is still in flight: the row spins and
    /// ticks only then. An all-finished run reads past tense with its
    /// measured total.
    pub running: bool,
}

/// B12: present-tense verb and target naming one tool call on a live row.
///
/// Q1: the progressive verb ("Running") names only a call still in
/// flight. A finished call reads in its settled tense — the card's own
/// verb, which every fold writes past-tense on completion ("Ran",
/// "Searched", "Read") — so a finished shell never reads "Running".
pub fn run_verb_target(call: &ToolCall) -> (String, String) {
    if call.status != ToolStatus::Running {
        return (call.verb.clone(), call.target.clone());
    }
    let target = call.target.clone();
    let verb = match &call.kind {
        ToolKind::Read => "Reading",
        ToolKind::Search => "Searching",
        ToolKind::Shell => "Running",
        ToolKind::Edit => "Editing",
        ToolKind::Write => "Writing",
        ToolKind::Web => "Loading",
        ToolKind::Browser => "Browsing",
        ToolKind::SubAgent => "Delegating",
        ToolKind::Mcp { .. } => "Using",
    };
    (verb.to_owned(), target)
}

/// B12fix: whether a tool call joins a live run: quiet reads, searches
/// and MCP calls. Edits, sub-agent delegations and failures keep their
/// own cards because the person may act on them. Shells join unless they
/// are still running past the ~2 s line — a finished command (even a slow
/// one) joins the run; only a command RUNNING long gets its own card, so
/// the wait reads as running, never hung.
fn call_joins_run(call: &ToolCall) -> bool {
    if call_failed(call) {
        return false;
    }
    match &call.kind {
        ToolKind::Read | ToolKind::Search | ToolKind::Mcp { .. } => true,
        ToolKind::Shell => {
            !(call.status == ToolStatus::Running && call.duration_ms.is_some_and(|ms| ms > 2_000))
        }
        ToolKind::Edit | ToolKind::Write | ToolKind::Web | ToolKind::Browser | ToolKind::SubAgent => {
            false
        }
    }
}

/// B12fix: whether a tool group joins a live run: every member joins on
/// its own. A group with an edit, a failure or a long-running command
/// keeps its own card; a quiet group (the muse lane's read runs) folds
/// into the run like its members would.
fn group_joins_run(calls: &[ToolCall]) -> bool {
    !calls.is_empty() && calls.iter().all(call_joins_run)
}

/// Q1: whether an approval card already shows a shell call's command, so
/// the call earns no separate live run row beside it. The card carries
/// its own request id, never the tool call's, so the join is the exact
/// command text: the bare command (`Bash`) or the terminal face's
/// backticked shape (`Run in terminal · \`<command>\``).
fn approval_covers(approval_command: &str, target: &str) -> bool {
    let target = target.trim();
    if target.is_empty() {
        return false;
    }
    let command = approval_command.trim();
    command == target || command == format!("Run in terminal · `{target}`")
}

/// Whether `block` is a shell call an approval for `command` stands for.
fn shell_call_for(block: Option<&Block>, command: &str) -> bool {
    block.and_then(|block| block.as_tool_call()).is_some_and(|call| {
        call.kind == ToolKind::Shell && approval_covers(command, &call.target)
    })
}

/// Q1b: the block indices of shell calls an approval card already shows —
/// one call per approval, never more. Each approval covers a single call:
/// the nearest matching call at or before the card (the live wire cards
/// the `tool_use` before its `can_use_tool`) else the first matching call
/// after it. A later re-run of the same command matches no unconsumed
/// approval, so it keeps its own row. The protocol's approval block
/// carries no tool-use id, so the text join is the whole join — but the
/// 1:1 consumption is what stops the duplicates.
fn covered_tool_blocks(blocks: &[Block]) -> HashSet<usize> {
    let approvals: Vec<(usize, &str)> = blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| match block {
            Block::Approval { command, .. } => Some((index, command.as_str())),
            _ => None,
        })
        .collect();
    let mut covered = HashSet::new();
    for (at, command) in approvals {
        let hit = (0..=at)
            .rev()
            .find(|index| !covered.contains(index) && shell_call_for(blocks.get(*index), command))
            .or_else(|| {
                ((at + 1)..blocks.len())
                    .find(|index| !covered.contains(index) && shell_call_for(blocks.get(*index), command))
            });
        if let Some(index) = hit {
            covered.insert(index);
        }
    }
    covered
}

/// B12fix: partition one turn's blocks into live runs: maximal runs of
/// consecutive joinable tool calls — quiet calls and quiet groups, even a
/// lone one, which still earns the live row. Edits, failures,
/// long-running commands and prose break the run and render as their own
/// rows. Only live turns collapse; settled turns fold instead (see
/// [`fold_plan`]).
pub fn live_runs(turn: &Turn, now_ms: u64, starts: &HashMap<String, u64>) -> Vec<LiveRun> {
    let Turn::Assistant { blocks, timestamp, .. } = turn else { return Vec::new() };
    // Q1b: one covered call per approval card, hidden outright — the card
    // is the row, never a second card beside it, and the hidden call
    // splits no run around itself.
    let covered = covered_tool_blocks(blocks);
    let mut runs = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let flush = |current: &mut Vec<usize>, runs: &mut Vec<LiveRun>| {
        if current.is_empty() {
            return;
        }
        let blocks_taken = std::mem::take(current);
        let mut run_ms = 0u64;
        let mut running = false;
        let mut live_call: Option<ToolCall> = None;
        let mut last_call: Option<ToolCall> = None;
        let mut first_id: Option<String> = None;
        for index in &blocks_taken {
            let member: Vec<ToolCall> = match blocks.get(*index) {
                Some(Block::ToolCall { .. }) => {
                    vec![blocks[*index].as_tool_call().expect("matched ToolCall")]
                }
                Some(Block::ToolGroup { calls, .. }) => calls.clone(),
                _ => Vec::new(),
            };
            for call in member {
                if first_id.is_none() {
                    first_id = Some(call.id.clone());
                }
                // Q1b: a finished member contributes its measured duration;
                // an unmeasured finished call contributes nothing.
                run_ms = run_ms.saturating_add(call.duration_ms.unwrap_or(0));
                if call.status == ToolStatus::Running {
                    running = true;
                    live_call = Some(call.clone());
                }
                last_call = Some(call);
            }
        }
        let Some(last) = last_call else { return };
        if running {
            // Q1b: a run with anything still in flight ticks from the
            // run's first member start — never the turn start plus
            // measured durations, which reads older than the turn itself.
            // Unknown ids fall back to the turn timestamp, and the tick
            // never exceeds the turn's own age.
            let first_start = first_id
                .as_deref()
                .and_then(|id| starts.get(id).copied())
                .or(*timestamp)
                .unwrap_or(now_ms);
            run_ms = now_ms.saturating_sub(first_start);
            if let Some(sent) = timestamp {
                run_ms = run_ms.min(now_ms.saturating_sub(*sent));
            }
        }
        // The last call still in flight speaks for the row; all finished
        // reads the last call's settled (past-tense) verb.
        let (verb, target) = run_verb_target(&live_call.unwrap_or(last));
        runs.push(LiveRun {
            earlier: blocks_taken.len() - 1,
            blocks: blocks_taken,
            verb,
            target,
            elapsed_ms: run_ms,
            running,
        });
    };
    for (index, block) in blocks.iter().enumerate() {
        // A covered call is no row at all: hidden like an empty Thinking
        // block, splitting no run around itself.
        if covered.contains(&index) {
            continue;
        }
        // An empty Thinking block takes no row and breaks no run: it is
        // not a row at all, so the run flows around it.
        if is_empty_thinking(block) {
            continue;
        }
        let joins = match block {
            Block::ToolCall { .. } => {
                block.as_tool_call().is_some_and(|call| call_joins_run(&call))
            }
            Block::ToolGroup { calls, .. } => group_joins_run(calls),
            _ => false,
        };
        if joins {
            current.push(index);
        } else {
            flush(&mut current, &mut runs);
        }
    }
    flush(&mut current, &mut runs);
    runs
}

/// B12: one addressable row of a mapped turn: the row counts and the row
/// renderer share this, so a click and a count never disagree. Selection
/// is per turn id (the library's [`turn_selected_text`]), not per row, so
/// folding moves rows without moving spans.
pub enum TurnSlot {
    /// The settled fold's header row.
    FoldHeader,
    /// One live run's collapsed row: the run's block indices.
    LiveRun(Vec<usize>),
    /// One block's own row.
    Block(usize),
    /// The silent-reasoning footer row.
    SilentFooter,
}

/// B12fix: the block indices of a turn that take rows: every block but
/// an empty Thinking block, which takes no row at all (live or settled).
fn shown_indices(blocks: &[Block]) -> Vec<usize> {
    blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| !is_empty_thinking(block))
        .map(|(index, _)| index)
        .collect()
}

/// Q1b: the block indices that take rows on a LIVE turn: shown blocks
/// minus the approval-covered calls, which render no row or card at all
/// (the approval card stands for them). Settled turns keep
/// [`shown_indices`] — the fold owns them, not the live mapping.
fn live_shown_indices(blocks: &[Block]) -> Vec<usize> {
    let covered = covered_tool_blocks(blocks);
    blocks
        .iter()
        .enumerate()
        .filter(|(index, block)| !covered.contains(index) && !is_empty_thinking(block))
        .map(|(index, _)| index)
        .collect()
}

/// B12: how many rows a turn occupies under folding: the unfolded count
/// ([`turn_rows`]) except for a settled turn with a fold plan (header +
/// visible + answer, or header + everything when open) and a live turn
/// with collapsed runs. `toggled` carries the person's open overrides;
/// `fold_enabled` is the "Fold finished turns" setting (default on).
/// B12fix: empty Thinking blocks take no row in any state, and the plan
/// comes from the per-turn cache.
pub fn turn_mapped_rows(
    turn: &Turn,
    settled: bool,
    toggled: &HashSet<String>,
    fold_enabled: bool,
) -> usize {
    let Turn::Assistant { id, blocks, .. } = turn else { return turn_rows(turn) };
    let silent = usize::from(silent_reasoning(turn).is_some());
    let shown = shown_indices(blocks).len();
    if settled && fold_enabled {
        if let Some(plan) = fold_plan_cached(turn, settled, toggled) {
            let open = toggled.contains(&fold_key(id));
            if open {
                return 1 + shown + silent;
            }
            return 1 + plan.visible.len() + usize::from(plan.answer.is_some()) + silent;
        }
        return shown + silent;
    }
    if !settled {
        // Q1b: the live mapping hides covered calls outright, so it
        // counts and walks the live-shown indices — never `shown`.
        let live_shown = live_shown_indices(blocks).len();
        let runs = live_runs(turn, 0, &HashMap::new());
        if runs.is_empty() {
            return live_shown + silent;
        }
        let mut rows = live_shown + silent;
        for run in &runs {
            let open = toggled.contains(&live_key(id, run.blocks[0]));
            rows -= run.blocks.len();
            rows += if open { 1 + run.blocks.len() } else { 1 };
        }
        return rows;
    }
    shown + silent
}

/// B12: which slot row `row` of a mapped turn is: the single mapping the
/// row counts ([`turn_mapped_rows`]) and [`turn_row`] share. `None` is a
/// stale row past the mapping — a list one frame ahead of its cache —
/// and renders nothing, like the old past-`turn_rows` guard.
pub fn turn_slot_at(
    turn: &Turn,
    settled: bool,
    toggled: &HashSet<String>,
    fold_enabled: bool,
    row: usize,
) -> Option<TurnSlot> {
    let Turn::Assistant { id, blocks, .. } = turn else {
        return (row == 0).then_some(TurnSlot::Block(0));
    };
    let silent_here = silent_reasoning(turn).is_some();
    // B12fix: rows only ever address shown blocks — an empty Thinking
    // block is skipped in every walk below, live or settled.
    let shown = shown_indices(blocks);
    if settled && fold_enabled {
        if let Some(plan) = fold_plan_cached(turn, settled, toggled) {
            let open = toggled.contains(&fold_key(id));
            if open {
                if row == 0 {
                    return Some(TurnSlot::FoldHeader);
                }
                let inner = row - 1;
                if inner < shown.len() {
                    return Some(TurnSlot::Block(shown[inner]));
                }
                return silent_here.then_some(TurnSlot::SilentFooter);
            }
            if row == 0 {
                return Some(TurnSlot::FoldHeader);
            }
            // B12fix: the closed fold keeps original order among the
            // visible blocks and the answer — the answer reads where the
            // turn put it, not appended after kept-visible blocks that
            // came after it.
            let mut slots: Vec<usize> = plan.visible.clone();
            if let Some(answer) = plan.answer {
                if !slots.contains(&answer) {
                    slots.push(answer);
                }
            }
            slots.sort_unstable();
            if row - 1 < slots.len() {
                return Some(TurnSlot::Block(slots[row - 1]));
            }
            return silent_here.then_some(TurnSlot::SilentFooter);
        }
        if row < shown.len() {
            return Some(TurnSlot::Block(shown[row]));
        }
        return (silent_here && row == shown.len()).then_some(TurnSlot::SilentFooter);
    }
    if !settled {
        // Q1b: covered calls take no row — the walk below is over the
        // live-shown indices, in which every run member lines up.
        let live_shown = live_shown_indices(blocks);
        let runs = live_runs(turn, 0, &HashMap::new());
        if !runs.is_empty() {
            // Walk the shown blocks, collapsing closed runs to one row and
            // expanding open runs to header + members.
            let mut at = 0usize;
            let mut run_at: HashMap<usize, &LiveRun> = HashMap::new();
            for run in &runs {
                run_at.insert(run.blocks[0], run);
            }
            let mut shown_at = 0usize;
            while shown_at < live_shown.len() {
                let index = live_shown[shown_at];
                if let Some(run) = run_at.get(&index) {
                    let open = toggled.contains(&live_key(id, run.blocks[0]));
                    if at == row {
                        return Some(TurnSlot::LiveRun(run.blocks.clone()));
                    }
                    at += 1;
                    if open {
                        if row >= at && row < at + run.blocks.len() {
                            return Some(TurnSlot::Block(run.blocks[row - at]));
                        }
                        at += run.blocks.len();
                    }
                    shown_at += run.blocks.len();
                } else {
                    if at == row {
                        return Some(TurnSlot::Block(index));
                    }
                    at += 1;
                    shown_at += 1;
                }
            }
            return (silent_here && at == row).then_some(TurnSlot::SilentFooter);
        }
        if row < live_shown.len() {
            return Some(TurnSlot::Block(live_shown[row]));
        }
        return (silent_here && row == live_shown.len()).then_some(TurnSlot::SilentFooter);
    }
    None
}

/// The reasoning tokens a finished turn billed without showing any work, if any.
///
/// A turn can bill a reasoning budget and emit no `reasoning` item at all —
/// the fold records the count on [`TurnMeta::reasoning_tokens`] either way.
/// Returns the count when the turn is an assistant turn with
/// `reasoning_tokens > 0` and no [`Block::Thinking`], and `None` otherwise
/// (user turns, no billed reasoning, or a visible trace that speaks for
/// itself).
pub fn silent_reasoning(turn: &Turn) -> Option<u64> {
    let Turn::Assistant { blocks, meta, .. } = turn else { return None };
    if meta.reasoning_tokens == 0 {
        return None;
    }
    if blocks.iter().any(|block| matches!(block, Block::Thinking { .. })) {
        return None;
    }
    Some(meta.reasoning_tokens)
}

/// The footer's reasoning cell for a turn [`silent_reasoning`] fired on.
///
/// The library footer draws `"419 reasoning"`, and a silent turn's cell
/// reads the same — the count with no thinking card behind it, on the same
/// line in the same style. Turns with a visible thinking card keep the
/// library's cell unchanged.
pub fn silent_reasoning_text(count: u64) -> String {
    format!("{count} reasoning")
}

/// The library footer's cells for a silent turn: a mirror of the library's
/// private `footer_items` (`aui/src/transcript/turns.rs`) with the reasoning
/// cell replaced by [`silent_reasoning_text`].
///
/// The library draws the footer from `TurnMeta` with no per-cell hook, so a
/// silent turn carries no `assistant_turn(..).meta(..)` and gets this row
/// instead — same cells, same order, same separators, same style. Keep in
/// sync with the library.
/// The footer model cell: a Claude Code wire id reads humanised
/// (`claude-haiku-4-5-20251001` → `Haiku 4.5`), never raw. Only
/// `claude-`-prefixed ids map — every other lane's model cells pass
/// through exactly as before.
fn footer_model_label(model: &str) -> String {
    if model.starts_with("claude-") {
        crate::providers::claude_code_model_label(model)
    } else {
        model.to_owned()
    }
}

fn silent_footer_items(meta: &TurnMeta, count: u64) -> Vec<String> {
    let tokens = meta.tokens_in + meta.tokens_out;
    // Zero is "the wire did not say", not "this turn was free" — dropped
    // here exactly as the library's `footer_items` drops it, and as the cost
    // cell below is dropped when the catalog reports no price. `items.retain`
    // takes the empty string out (audit 2026-09-13).
    let tokens = match tokens {
        0 => String::new(),
        n if n >= 1000 => format!("{:.1}k tokens", n as f64 / 1000.0),
        n => format!("{n} tokens"),
    };
    let mut items = vec![
        footer_model_label(&meta.model),
        if meta.duration_ms == 0 { String::new() } else { format!("{:.1} s", meta.duration_ms as f64 / 1000.0) },
        tokens,
    ];
    items.retain(|item| !item.is_empty());
    items.push(silent_reasoning_text(count));
    if meta.cost_usd > 0.0 {
        items.push(format!("${:.2}", meta.cost_usd));
    }
    items
}

/// The single footer line for a silent turn: the library footer's own row
/// (mono, `FS_11`, `ink_4`, `·` separators) with the silent reasoning cell,
/// plus the turn's age when the wire timed it.
fn silent_footer_row(meta: &TurnMeta, count: u64, age: Option<String>, cx: &mut App) -> AnyElement {
    use aui_tokens::{ActiveAui, AuiStyled};
    let p = cx.aui().colors;
    let mut footer = h_flex()
        .w_full()
        .mt(px(10.0))
        .gap(px(10.0))
        .font_family(scale::FONT_MONO)
        .text_px(scale::FS_11)
        .line_height(relative(1.0))
        .medium()
        .text_color(p.ink_4);
    let mut items = silent_footer_items(meta, count);
    if let Some(age) = age {
        items.push(age);
    }
    for (i, item) in items.into_iter().enumerate() {
        if i > 0 {
            footer = footer.child("·");
        }
        footer = footer.child(item);
    }
    footer.into_any_element()
}

/// The clock a frame formats turn ages against: wall time, except under
/// `BAAZ_DETERMINISTIC=1`, where it is the newest reported timestamp in
/// the data — so a `--replay … --screenshot` capture reads the same words
/// run to run however old the fixture is. The same discipline as
/// [`crate::sidebar::grouping_now`], the sibling formatter's clock.
pub fn transcript_now_ms(turns: &[Rc<Turn>]) -> u64 {
    if crate::clock::deterministic() {
        turns
            .iter()
            .filter_map(|turn| turn.timestamp())
            .max()
            .unwrap_or_else(wall_now_ms)
    } else {
        wall_now_ms()
    }
}

/// Wall time as Unix milliseconds. The fallback when no turn reported a
/// timestamp, and the whole clock outside deterministic captures.
fn wall_now_ms() -> u64 {
    chrono::Local::now().timestamp_millis().max(0) as u64
}

/// How-long-ago words for a turn's timestamp: `just now`, `N minutes ago`,
/// `N hours ago`, `yesterday`, else the calendar date (`Sep 8`, with the
/// year when it is not this one).
///
/// The sibling of the sidebar's `elapsed_at` and the library's `format_age`:
/// the same explicit-clock discipline — the caller reads its clock once per
/// frame ([`transcript_now_ms`]) and hands both instants in, so one frame
/// formats once — the same quantisation against the same saturation (a stamp
/// from the future reads `just now`), with the words a caption needs.
pub fn relative_words(sent_ms: u64, now_ms: u64) -> String {
    let seconds = now_ms.saturating_sub(sent_ms) / 1000;
    match seconds {
        s if s < 60 => "just now".to_owned(),
        s if s < 3_600 => {
            let minutes = s / 60;
            if minutes == 1 {
                "1 minute ago".to_owned()
            } else {
                format!("{minutes} minutes ago")
            }
        }
        s if s < 86_400 => {
            let hours = s / 3_600;
            if hours == 1 {
                "1 hour ago".to_owned()
            } else {
                format!("{hours} hours ago")
            }
        }
        s if s < 172_800 => "yesterday".to_owned(),
        _ => {
            use chrono::{Datelike, TimeZone};
            let sent = chrono::Local.timestamp_millis_opt(sent_ms as i64).single();
            let now = chrono::Local.timestamp_millis_opt(now_ms as i64).single();
            match (sent, now) {
                (Some(sent), Some(now)) if sent.year() == now.year() => sent.format("%b %-d").to_string(),
                (Some(sent), Some(_)) => sent.format("%b %-d, %Y").to_string(),
                _ => "older".to_owned(),
            }
        }
    }
}

/// The age caption for a turn with a reported timestamp, else `None` — a
/// turn the wire never timed draws no caption and keeps its old height.
fn turn_age(timestamp: Option<u64>, now_ms: u64) -> Option<String> {
    timestamp.map(|sent| relative_words(sent, now_ms))
}

/// How many transcript rows a turn occupies: one for the person's bubble;
/// one per block of an assistant reply, plus Baaz's own footer row
/// on a silent turn.
///
/// The virtual list is one item per **row**, not per turn (see
/// `SessionView::sync_virtual_list`): a real turn can run to hundreds of
/// blocks, and a list item is laid out whole every frame it is visible, so
/// per-turn items made a frame cost what the biggest visible turn cost.
pub fn turn_rows(turn: &Turn) -> usize {
    match turn {
        Turn::User { .. } => 1,
        Turn::Assistant { blocks, .. } => blocks.len() + usize::from(silent_reasoning(turn).is_some()),
    }
}

/// Render one row of a turn — see [`turn_rows`] for what a row is.
///
/// `settled` is false only for the newest turn, so history does not replay the
/// reveal animation when the window opens or a session is resumed. A `row`
/// past [`turn_rows`] renders nothing, so a list that is one frame ahead of
/// its cache never panics.
pub fn turn_row(turn: &Turn, row: usize, settled: bool, folds: &Folds, window: &mut Window, cx: &mut App) -> AnyElement {
    // A turn can bill reasoning tokens and emit no reasoning item at all
    // (improvement candidate 3 in docs/09-handoff-improvements.md §8). On a
    // silent turn this row *is* the footer — the library's cells with the
    // reasoning cell saying what the count meant — so the library must not
    // draw its own underneath.
    let silent = silent_reasoning(turn);
    match turn {
        Turn::User { id, text, timestamp, .. } => {
            if row != 0 {
                return div().into_any_element();
            }
            let mut turn = user_turn(SharedString::from(id.clone()), text.clone()).actions_bottom(true);
            // The how-long-ago caption under the bubble, beside the action
            // rail. A turn the wire never timed draws no caption and keeps
            // its old height.
            if let Some(age) = turn_age(*timestamp, folds.now_ms) {
                turn = turn.age(age);
            }
            if folds.copied.contains(id) {
                turn = turn.copied(true);
            }
            if let Some(on_link) = &folds.link {
                let on_link = on_link.clone();
                turn = turn.on_link(move |target, window, cx| on_link(target, window, cx));
            }
            // The turn's own held span, if any (C8b): every turn gets
            // only its own, because cell keys repeat across turns. The
            // span path replaces the legacy single-cell selection — a drag
            // that starts in one paragraph and ends in another, or in a
            // code block, highlights everything between.
            turn = turn.span_selection(folds.span_held.get(id).map(|(_, held)| held));
            if let Some(on_event) = &folds.span_event {
                let on_event = on_event.clone();
                let (turn_id, source) = (id.clone(), text.clone());
                turn = turn.on_span_event(move |event, window, cx| {
                    on_event(turn_id.clone(), source.clone(), event, window, cx);
                });
            }
            if let Some(act) = &folds.user_action {
                let act = act.clone();
                let (turn_id, body) = (id.clone(), text.clone());
                turn = turn
                    .on_action(move |action, window, cx| act(turn_id.clone(), body.clone(), action, window, cx));
            }
            // A plain block parent, not a flex row: aui's user column is
            // `w_full` capped at 78% with `ml_auto`, which only resolves
            // to a definite, right-anchored width as a block child. As a
            // flex item the column shrink-wraps and the bubble inside
            // clips to a few characters (aui v0.3.x `ml_auto` addition).
            div().w_full().child(turn).into_any_element()
        }
        Turn::Assistant { id, blocks, meta, timestamp, .. } => {
            // B12: rows go through the shared slot mapping, so counts and
            // clicks agree whether the fold is open or closed, live or
            // settled. A stale row past the mapping renders nothing, like
            // the old past-`turn_rows` guard.
            let answer = fold_plan_cached(turn, settled, &folds.toggled).and_then(|plan| plan.answer);
            // A silent turn gets Baaz's own footer row, so its blocks
            // carry no library footer.
            let library_meta = if silent.is_some() { None } else { Some(meta) };
            let render_block = |index: usize, window: &mut Window, cx: &mut App| {
                let Some(b) = blocks.get(index) else {
                    return div().into_any_element();
                };
                let key = block_key(id, index);
                let reveal = stream_reveal(ElementId::from(SharedString::from(key.clone())), row, settled, window, cx);
                // The turn's closing text block carries the token footer;
                // under a fold that is the final answer wherever it sits.
                let last = answer.map_or(index == blocks.len().saturating_sub(1), |a| a == index);
                let body = block(&key, id, b, last, library_meta, *timestamp, folds, cx);
                div().w_full().relative().top(reveal.offset_y).opacity(reveal.opacity).child(body).into_any_element()
            };
            match turn_slot_at(turn, settled, &folds.toggled, folds.fold_finished_turns, row) {
                Some(TurnSlot::FoldHeader) => {
                    let plan = fold_plan_cached(turn, settled, &folds.toggled).expect("header implies a plan");
                    let open = folds.open(&fold_key(id), false);
                    fold_header_row(id, &plan, open, folds)
                }
                Some(TurnSlot::LiveRun(indices)) => {
                    let start = indices.first().copied().unwrap_or(0);
                    let open = folds.open(&live_key(id, start), false);
                    match live_runs(turn, folds.now_ms, &folds.tool_starts)
                        .into_iter()
                        .find(|run| run.blocks.first() == Some(&start))
                    {
                        Some(run) => live_run_row(id, &run, open, folds),
                        None => render_block(start, window, cx),
                    }
                }
                Some(TurnSlot::Block(index)) => render_block(index, window, cx),
                Some(TurnSlot::SilentFooter) => match silent {
                    Some(count) => silent_footer_row(meta, count, turn_age(*timestamp, folds.now_ms), cx),
                    None => div().into_any_element(),
                },
                None => div().into_any_element(),
            }
        }
    }
}

/// B12: one settled turn's fold header row: "Worked for 2m 14s · read 12
/// files, edited 3, ran 5 commands" with the `+N −M` chip opening
/// Changes. The header toggles the fold; the chip opens the turn's diff.
/// No new chrome is added here — the library owns the row's roles and
/// labels — only the two intents are wired.
fn fold_header_row(turn_id: &str, plan: &FoldPlan, open: bool, folds: &Folds) -> AnyElement {
    use aui_protocol::DiffStat;
    let id = ElementId::from(SharedString::from(format!("{turn_id}:fold")));
    let diff = (plan.diff_added + plan.diff_removed > 0).then_some(DiffStat {
        added: plan.diff_added,
        removed: plan.diff_removed,
        files: 0,
    });
    let toggle = folds.toggle.clone();
    let key = fold_key(turn_id);
    let open_diff = folds.open_turn_diff.clone();
    let turn_id = turn_id.to_owned();
    turn_fold(id, plan.elapsed_ms, plan.reads, plan.edits, plan.commands)
        .diff_stat(diff)
        .open(open)
        .on_intent(move |intent, window, cx| match intent {
            TurnFoldIntent::Toggle => toggle(key.clone(), window, cx),
            TurnFoldIntent::OpenDiff => {
                if let Some(open_diff) = &open_diff {
                    open_diff(turn_id.clone(), window, cx);
                }
            }
        })
        .into_any_element()
}

/// B12: one live run's collapsed row: the current call's verb and target
/// with "+N earlier" and the run's elapsed. Clicking expands to the run's
/// cards, which the row mapping then hosts as following rows.
fn live_run_row(turn_id: &str, run: &LiveRun, open: bool, folds: &Folds) -> AnyElement {
    let start = run.blocks.first().copied().unwrap_or(0);
    let id = ElementId::from(SharedString::from(format!("{turn_id}:live:{start}")));
    let toggle = folds.toggle.clone();
    let key = live_key(turn_id, start);
    live_activity_row(id, run.verb.clone(), run.target.clone(), run.earlier, run.elapsed_ms)
        .open(open)
        .on_toggle(move |_, window, cx| toggle(key.clone(), window, cx))
        .into_any_element()
}

/// B12fix: the one-line text an Artifact tool folds to: `published
/// <title or url>` when the result names what it made, else the tool's
/// own kind (`quickstart`). Pure, so the folds and the tests share it —
/// the card never inlines the artifact body.
pub fn artifact_summary_text(detail: &str) -> String {
    let detail = detail.trim();
    if detail.is_empty() {
        return "Artifact · quickstart".to_owned();
    }
    let first = detail.lines().next().unwrap_or("").trim();
    if first.is_empty() {
        return "Artifact · quickstart".to_owned();
    }
    format!("Artifact · published {first}")
}

/// B12: the fallback card for an unknown item kind: collapsed to a preview
/// by default, with a working chevron and "Open full text" into the right
/// pane's read-only doc view.
fn generic_card(key: &str, id: ElementId, kind: &str, status: &str, text: &str, folds: &Folds) -> AnyElement {
    let mut card = generic_item_card(id, kind.to_owned(), status.to_owned(), text.to_owned())
        .open(folds.open(key, false));
    let toggle = folds.toggle.clone();
    let key = key.to_owned();
    let title = kind.to_owned();
    let body = text.to_owned();
    let open_full = folds.open_full_text.clone();
    let (full_title, full_body) = (title.clone(), body.clone());
    card = card.on_intent(move |intent, window, cx| match intent {
        GenericItemIntent::Toggle => toggle(key.clone(), window, cx),
        GenericItemIntent::OpenFull => {
            if let Some(open_full) = &open_full {
                open_full(title.clone(), body.clone(), window, cx);
            } else {
                toggle(key.clone(), window, cx);
            }
        }
    });
    if let Some(open_full) = &folds.open_full_text {
        let open_full = open_full.clone();
        card = card.on_open_full(move |window, cx| open_full(full_title.clone(), full_body.clone(), window, cx));
    }
    card.into_any_element()
}

/// B12: the whole text an open-in-pane fold row offers: search hits, MCP
/// results and shell output open in the right pane's doc view instead of
/// inlining past a few lines. `None` is a card with nothing worth opening
/// (reads, bare headers), which keeps its toggle.
fn tool_pane_text(call: &ToolCall) -> Option<(String, String)> {
    let title = if call.target.trim().is_empty() {
        call.verb.clone()
    } else {
        format!("{} {}", call.verb, call.target)
    };
    match &call.body {
        ToolBody::Search { hits } => {
            let text = hits
                .iter()
                .map(|hit| format!("{}:{}: {}", hit.path, hit.line, hit.snippet))
                .collect::<Vec<_>>()
                .join("\n");
            Some((title, text))
        }
        ToolBody::Mcp { result_json, .. } => Some((title, result_json.clone())),
        ToolBody::Shell { output_lines, .. } => Some((title, output_lines.join("\n"))),
        ToolBody::Web { results, .. } => {
            let text = results
                .iter()
                .map(|result| format!("{} — {}", result.title, result.url))
                .collect::<Vec<_>>()
                .join("\n");
            Some((title, text))
        }
        ToolBody::Read { .. }
        | ToolBody::Edit { .. }
        | ToolBody::Browser { .. }
        | ToolBody::SubAgent { .. }
        | ToolBody::None => None,
    }
}

/// One block of an assistant turn.
///
/// `last` and `meta` exist for one reason: the per-turn token footer belongs
/// under the reply, and [`assistant_turn`] is the component that draws it, so
/// the turn's closing text block is the one that carries the meta. `meta` is
/// `None` on a silent turn, which gets Baaz's own footer row instead
/// (see [`silent_footer_row`]): the library must not draw its own underneath.
/// `timestamp` is the turn's wire time, carried so the closing block can sign
/// off with the same age caption the user bubble draws.
#[allow(clippy::too_many_arguments)]
fn block(
    key: &str,
    turn_id: &str,
    block: &Block,
    last: bool,
    meta: Option<&TurnMeta>,
    timestamp: Option<u64>,
    folds: &Folds,
    cx: &mut App,
) -> AnyElement {
    let id = ElementId::from(SharedString::from(key.to_owned()));
    match block {
        Block::Text { text, streaming } => text_card(id, turn_id, text, *streaming, last, meta, timestamp, folds, cx),
        Block::Thinking { text, elapsed_ms, summary, state } => {
            thinking_card(id, key, text, *elapsed_ms, summary.as_deref(), *state, folds)
                .unwrap_or_else(|| div().into_any_element())
        }
        Block::Activity { steps, summary, elapsed_ms, state } => {
            activity_card(id, key, steps, summary, *elapsed_ms, *state, folds)
        }
        Block::ToolCall { .. } => match block.as_tool_call() {
            Some(call) => match skill_load_name(&call) {
                Some(name) => skill_load_row(id, name, folds, cx),
                None => tool_call_card(key, id, &call, folds),
            },
            None => generic_card(key, id, "tool", "done", "", folds),
        },
        Block::ToolGroup { .. } => tool_group_card(id, key, block, folds),
        Block::Approval { .. } => approval_block_card(id, block, folds),
        Block::Question { .. } => question_block_card(id, block, folds),
        Block::Plan { id: plan_id, items, sections, state } => {
            plan_block_card(id, plan_id, items, sections, *state, folds)
        }
        Block::Todo { items } => {
            todo_list(id, items.clone()).open(folds.open(key, true)).on_toggle(fold_toggle(key, folds)).into_any_element()
        }
        Block::Summary { title, files, checks, duration_ms, cost_usd } => {
            summary_card(id, title.clone(), format!("{} · ${cost_usd:.2}", elapsed(*duration_ms)))
                .files(files.clone())
                .checks(checks.clone())
                .into_any_element()
        }
        Block::Error { title, detail, retryable } => {
            error_block_card(id, turn_id, title, detail, *retryable, folds)
        }
        Block::Goal { objective, status, percent_complete, current_work, next_work } => {
            goal_block_card(id, objective, status, *percent_complete, current_work.as_deref(), next_work.as_deref())
        }
        // MSP's item kinds are an open set and mandate exactly this fallback:
        // the kind, the status and the server's own one-line text — B12
        // collapsed to a preview with a working chevron and "Open full
        // text" into the right pane.
        // B12fix: the Artifact tool reads as a one-line summary card,
        // never the generic dump — the body opens in the pane on demand.
        Block::Generic { kind, status, text } if kind == "Artifact" => {
            generic_card(key, id, kind, status, &artifact_summary_text(text), folds)
        }
        Block::Generic { kind, status, text } => generic_card(key, id, kind, status, text, folds),
        Block::Marker { kind, text } => marker(id, kind, text, folds, cx),
        Block::Handoff { .. } => handoff_block_card(id, block, folds),
    }
}

/// One handoff card: the move's state, its progress steps, what the pack
/// carried and what it left behind, and the two intents the card raises
/// ("Open the new session", "Cancel"). Read-only without [`Folds::handoff`].
///
/// The protocol card carries no step list, so the steps re-derive here
/// from the block's own fields through [`handoff_steps`]: the same
/// mapping [`crate::handoff::HandoffRun::steps`] uses. The wait reads off
/// the Checkpointed shape (extractive kind, no destination yet); the
/// current step's elapsed counter reads the run's age off the card id's
/// creation stamp against this frame's clock, so it ticks on any
/// re-render — a stamp-less id simply shows no counter. Every step row
/// already carries its role and label from the library.
fn handoff_block_card(id: ElementId, block: &Block, folds: &Folds) -> AnyElement {
    let Block::Handoff {
        id: handoff_id,
        from,
        to,
        from_model,
        to_model,
        state,
        carried,
        lost,
        pack_tokens,
        destination_session,
    } = block
    else {
        return div().into_any_element();
    };
    let summary_kind = carried
        .first()
        .and_then(|item| item.detail.as_deref())
        .and_then(|detail| {
            if detail == SummaryKind::Model.label() {
                Some(SummaryKind::Model)
            } else if detail == SummaryKind::Extractive.label() {
                Some(SummaryKind::Extractive)
            } else {
                None
            }
        });
    let waiting = matches!(state, HandoffState::Checkpointed)
        && !matches!(summary_kind, Some(SummaryKind::Model))
        && destination_session.is_none();
    let elapsed = handoff_card_started_ms(handoff_id)
        .map(|started| folds.now_ms.saturating_sub(started) / 1000);
    let steps = handoff_steps(
        state,
        *to,
        summary_kind,
        waiting,
        destination_session.is_some(),
        !carried.is_empty(),
        elapsed,
    );
    let card = handoff_card(id, *from, *to, to_model.clone(), state.clone())
        .from_model(from_model.clone())
        .carried(carried.clone())
        .lost(lost.clone())
        .pack_tokens(*pack_tokens)
        .steps(steps)
        .destination_session(destination_session.clone());
    let Some(handoff) = &folds.handoff else { return card.into_any_element() };
    let (open, cancel) = (handoff.open.clone(), handoff.cancel.clone());
    // The Cancel intent names no card — the block id rides in the closure
    // the card captured, which is this render's own handoff id.
    let handoff_id = handoff_id.clone();
    card.on_intent(move |intent, window, cx| match intent {
        HandoffIntent::OpenSession(destination) => open(destination, window, cx),
        HandoffIntent::Cancel => cancel(handoff_id.clone(), window, cx),
    })
    .into_any_element()
}

/// A skill load's quiet one-line row (D63): muted text, no card chrome, no
/// fold toggle — a load is a fact, not a conversation. Tapping it opens the
/// Skills page on that skill; without a handler it reads as plain text.
fn skill_load_row(id: ElementId, name: &str, folds: &Folds, cx: &mut App) -> AnyElement {
    use aui_tokens::{ActiveAui, AuiStyled};
    let p = cx.aui().colors;
    let text = skill_load_text(name, folds.skill_scopes.get(name).map(String::as_str));
    let row = h_flex()
        .w_full()
        .items_center()
        .gap(px(scale::SP_2))
        .py(px(2.0))
        .ui(scale::FS_13)
        .text_color(p.ink_3)
        .child(text);
    match &folds.open_skill {
        Some(open) => {
            let open = open.clone();
            let skill = name.to_owned();
            let label = format!("Open skill {skill} in the Skills page.");
            row.id(id)
                .cursor_pointer()
                .role(gpui::Role::Button)
                .aria_label(label)
                .on_click(move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
                    open(skill.clone(), window, cx);
                })
                .into_any_element()
        }
        None => row.into_any_element(),
    }
}

/// The fold toggle every collapsible card hangs off: one click, one key.
fn fold_toggle(key: &str, folds: &Folds) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    let toggle = folds.toggle.clone();
    let key = key.to_owned();
    move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| toggle(key.clone(), window, cx)
}

/// The reply itself, and on the closing block the turn's footer and actions.
///
/// A finished turn signs off with its footer; a running one has no final
/// numbers to show yet, and neither has a turn the server measured nothing
/// for. A silent turn carries no library footer — `meta` is `None` there —
/// because Baaz draws its own row.
#[allow(clippy::too_many_arguments)]
fn text_card(
    id: ElementId,
    turn_id: &str,
    text: &str,
    streaming: bool,
    last: bool,
    meta: Option<&TurnMeta>,
    timestamp: Option<u64>,
    folds: &Folds,
    cx: &mut App,
) -> AnyElement {
    // Pin has no meaning on a turn — it lives on sidebar sessions — so the
    // row keeps copy, retry and fork only.
    let mut turn = assistant_turn(id.clone(), text.to_owned())
        .actions(&[AssistantTurnAction::Copy, AssistantTurnAction::Retry, AssistantTurnAction::Fork])
        .streaming(streaming)
        .actions_bottom(last);
    if let Some(meta) = meta {
        if last && !streaming && meta != &TurnMeta::default() {
            // The library draws the footer from the meta as handed over,
            // so the humanised label maps here in baaz — never a raw id.
            let mut mapped = meta.clone();
            mapped.model = footer_model_label(&mapped.model);
            turn = turn.meta(mapped);
        }
    }
    // The how-long-ago cell at the end of the footer, beside the action
    // rail — but only on the closing block, and never without a wire time,
    // so an untimed turn keeps its old footer and its old height. A silent
    // turn carries no library footer (`meta` is `None` there): its age rides
    // Baaz's own footer row instead, so it is not drawn twice.
    if last {
        if meta.is_some() {
            if let Some(age) = turn_age(timestamp, folds.now_ms) {
                turn = turn.age(age);
            }
        }
        if folds.copied.contains(turn_id) {
            turn = turn.copied(true);
        }
    }
    if let Some(on_link) = &folds.link {
        let on_link = on_link.clone();
        turn = turn.on_link(move |target, window, cx| on_link(target, window, cx));
    }
    // The turn's own held span, if any (C8b). Sibling text blocks share
    // the turn id, so a span over one block's `p0` also tints the other's
    // — the library scopes keys to the markdown view, and a turn holds
    // several. The source that travels back with an event is still exactly
    // this block's text, so ⌘C copies what was dragged.
    turn = turn.span_selection(folds.span_held.get(turn_id).map(|(_, held)| held));
    if let Some(on_event) = &folds.span_event {
        let on_event = on_event.clone();
        let (owner, source) = (turn_id.to_owned(), text.to_owned());
        turn = turn.on_span_event(move |event, window, cx| {
            on_event(owner.clone(), source.clone(), event, window, cx);
        });
    }
    // The row belongs to the message, so only the closing block carries it.
    if last {
        if let Some(act) = &folds.assistant_action {
            let act = act.clone();
            let turn_id = turn_id.to_owned();
            turn = turn.on_action(move |action, window, cx| act(turn_id.clone(), action, window, cx));
        }
    }
    let body = turn.into_any_element();
    // D49's "Run" row: one button per runnable fence, in fence order. The
    // library's markdown view carries no per-fence action slot, so the row
    // rides under the turn, in the same transcript row — the turn itself
    // renders exactly as before, and a turn with no runnable fence renders
    // no row at all.
    match (&folds.terminal_run, runnable_blocks(text)) {
        (Some(run), blocks) if !blocks.is_empty() => {
            v_flex().child(body).child(code_run_row(&id, &blocks, run, cx)).into_any_element()
        }
        _ => body,
    }
}

/// The "Run" row under an assistant turn with runnable fences (D49): one
/// ghost button per fence with the command's first line beside it. A press
/// runs what [`runnable_command`] returned for that fence — for a `console`
/// block the prompt lines, prompts stripped, not the raw fence — pasting
/// the whole command; ⌥-click pastes without Enter.
fn code_run_row(
    id: &ElementId,
    blocks: &[RunnableBlock],
    run: &TerminalRunHandler,
    cx: &mut App,
) -> AnyElement {
    use aui_tokens::{ActiveAui, AuiStyled};
    let p = cx.aui().colors;
    let mut row = v_flex().w_full().pt(px(4.0)).gap(px(2.0));
    for (index, block) in blocks.iter().enumerate() {
        let run = run.clone();
        let command = block.command.clone();
        let label = block.label.clone();
        row = row.child(
            h_flex()
                .w_full()
                .items_center()
                .gap(px(8.0))
                .child(
                    button((id.clone(), SharedString::from(format!("run-{index}"))), RUN_LABEL)
                        .xs()
                        .ghost()
                        .icon(IconName::Play)
                        // The human label names the control for the
                        // accessibility tree (V1): one Run button per
                        // fence, each named for the command it runs.
                        .accessibility_label(SharedString::from(format!("Run {label}")))
                        .on_click(move |_, window, cx| {
                            if let Some(request) = RunRequest::new(
                                command.clone(),
                                send_enter_for_alt(window.modifiers().alt),
                            ) {
                                run(request, window, cx);
                            }
                        }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .truncate()
                        .mono(scale::FS_12)
                        .text_color(p.ink_3)
                        .child(SharedString::from(label)),
                ),
        );
    }
    row.into_any_element()
}

/// The reasoning trace: a live one stays open, a finished one collapses to
/// its summary until the person asks for it.
///
/// A trace with no text renders nothing: providers bill redacted thoughts
/// (a signature with no utterance) that must never become an empty
/// "Thought for 0.0 s" shell, on any lane.
fn thinking_card(
    id: ElementId,
    key: &str,
    text: &str,
    elapsed_ms: u64,
    summary: Option<&str>,
    state: ThinkingState,
    folds: &Folds,
) -> Option<AnyElement> {
    if text.trim().is_empty() {
        return None;
    }
    let done = state == ThinkingState::Done;
    let mut card = thinking_block(id, text.to_owned(), elapsed(elapsed_ms), state)
        .expanded(folds.open(key, !done))
        .on_toggle(fold_toggle(key, folds));
    if let Some(summary) = summary {
        card = card.summary(summary.to_owned());
    }
    Some(card.into_any_element())
}

/// A run of small steps, folded to one line by default.
fn activity_card(
    id: ElementId,
    key: &str,
    steps: &[Step],
    summary: &str,
    elapsed_ms: u64,
    state: ActivityState,
    folds: &Folds,
) -> AnyElement {
    activity_group(id, steps.to_vec(), summary.to_owned(), elapsed(elapsed_ms), state)
        .open(folds.open(key, false))
        .on_toggle(fold_toggle(key, folds))
        .into_any_element()
}

/// A grouped run through the library's group card (C8): the header toggles
/// the group, and the open group renders every call as the full card the lone
/// `Block::ToolCall` would have shown — same toggles, same full-output
/// fetches, keyed stably per call.
/// Z6b: a grouped block's display copy: every call's header target — and
/// an Edit body diff's path — shortened through [`display_path`]. The
/// fold's own block is untouched: group intents carry the call index, so
/// reveals resolve the full target from the stored call.
fn display_group_block(block: &Block, workspace_root: &str, home: &str) -> Block {
    let Block::ToolGroup { calls, summary, state } = block else {
        return block.clone();
    };
    let calls = calls
        .iter()
        .map(|call| {
            let mut call = call.clone();
            if !matches!(call.kind, ToolKind::Shell) {
                call.target = display_path(&call.target, workspace_root, home);
            }
            if let ToolBody::Edit { diff } = &mut call.body {
                diff.path = display_path(&diff.path, workspace_root, home);
            }
            call
        })
        .collect();
    Block::ToolGroup { calls, summary: summary.clone(), state: *state }
}

fn tool_group_card(id: ElementId, key: &str, block: &Block, folds: &Folds) -> AnyElement {
    let home = home_dir();
    let shown = display_group_block(block, &folds.workspace_root, &home);
    let Some(data) = ToolGroupData::from_block(&shown) else {
        return generic_card(key, id, "tool", "done", "", folds);
    };
    let mut group = tool_group(id, &data, folds.open(key, false));
    for (index, _) in data.calls.iter().enumerate() {
        group = group.call_open(index, folds.open(&format!("{key}:{index}"), true));
    }
    if let Some(handler) = &folds.tool_group {
        let handler = handler.clone();
        let key = key.to_owned();
        group = group.on_intent(move |intent, window, cx| handler(key.clone(), intent, window, cx));
    }
    group.into_any_element()
}

/// One approval, with whatever the app has decided about it: the open
/// feedback field, its text, and the two intents the card raises.
fn approval_block_card(id: ElementId, block: &Block, folds: &Folds) -> AnyElement {
    let Block::Approval {
        id: approval_id,
        tool,
        command,
        reason,
        cwd,
        capabilities,
        scope,
        body_kind,
        state,
        rule,
        choices,
        stages,
        current_stage,
        badges,
        feedback,
        resolved_by,
    } = block
    else {
        return div().into_any_element();
    };
    let mut card = approval_card(id, tool.clone(), command.clone(), state.clone())
        .title(approval_title(folds.approval_host.as_deref(), tool))
        .reason(reason.clone())
        .cwd(cwd.clone())
        .capabilities(capabilities.clone())
        .scope(*scope)
        .body_kind(*body_kind)
        .rule(rule.clone().unwrap_or_default())
        .choices(choices.clone())
        .stages(stages.clone(), *current_stage)
        .badges(*badges)
        .resolved_by(*resolved_by);
    if folds.at_rest {
        card = card.at_rest();
    }
    if let Some(feedback) = feedback {
        card = card.feedback(feedback.clone());
    }
    let Some(cards) = &folds.cards else { return card.into_any_element() };
    let open_here = cards.feedback_open.as_ref().filter(|(a, _)| a == approval_id);
    if let Some((_, choice_id)) = open_here {
        card = card.feedback_open(Some(choice_id.clone())).feedback_text(cards.feedback_text.clone());
        if let Some(slot) = cards.feedback_slot.borrow_mut().take() {
            card = card.feedback_slot(slot);
        }
    }
    let choose = cards.choose.clone();
    let toggle_feedback = cards.feedback_toggle.clone();
    let (a, b) = (approval_id.clone(), approval_id.clone());
    card.on_choose(move |choice_id, feedback, window, cx| choose(a.clone(), choice_id, feedback, window, cx))
        .on_feedback_toggle(move |choice_id, window, cx| toggle_feedback(b.clone(), choice_id, window, cx))
        .into_any_element()
}

/// One question. An answered one is a settled row rather than a card.
fn question_block_card(id: ElementId, block: &Block, folds: &Folds) -> AnyElement {
    let Block::Question {
        id: question_id,
        header,
        prompt,
        subtitle,
        options,
        multi,
        allow_other,
        answer,
        timeout_ms,
    } = block
    else {
        return div().into_any_element();
    };
    if let Some(answer) = answer {
        return settled_row(id, options, answer);
    }
    let mut card = question_card(id, prompt.clone(), options)
        .header(header.clone())
        .subtitle(subtitle.clone())
        .multi(*multi)
        .allow_other(*allow_other);
    // The deadline is the block's, so the pill is drawn whether or not
    // anything is wired up to answer; the *countdown* below needs the app's
    // clock and replaces it.
    if let Some(total) = timeout_ms {
        card = card.timeout(*total, *total);
    }
    let Some(cards) = &folds.cards else { return card.into_any_element() };
    if let Some(selected) = cards.selections.get(question_id) {
        card = card.selected(selected.clone());
    }
    if let Some(open) = cards.previews.get(question_id) {
        card = card.previews_open(open.clone());
    }
    // The card draws the pill; the clock is the app's, because MSP sends a
    // duration and never a deadline.
    if let Some((remaining, total)) = cards.countdowns.get(question_id) {
        card = card.timeout(*remaining, *total);
    }
    if cards.clarify_open.as_deref() == Some(question_id.as_str()) {
        card = card.clarify_open(true);
        if let Some(slot) = cards.clarify_slot.borrow_mut().take() {
            card = card.clarify_slot(slot);
        }
    }
    let (select, answer, skip, clarify, preview) = (
        cards.select.clone(),
        cards.answer.clone(),
        cards.skip.clone(),
        cards.clarify.clone(),
        cards.toggle_preview.clone(),
    );
    let ids = std::iter::repeat_n(question_id.clone(), 5).collect::<Vec<_>>();
    card.on_select({
        let id = ids[0].clone();
        move |index, window, cx| select(id.clone(), index, window, cx)
    })
    .on_toggle_preview({
        let id = ids[1].clone();
        move |index, window, cx| preview(id.clone(), index, window, cx)
    })
    .on_answer({
        let id = ids[2].clone();
        move |_, window, cx| answer(id.clone(), window, cx)
    })
    .on_skip({
        let id = ids[3].clone();
        move |_, window, cx| skip(id.clone(), window, cx)
    })
    .on_clarify({
        let id = ids[4].clone();
        move |_, window, cx| clarify(id.clone(), window, cx)
    })
    .into_any_element()
}

/// A plan, with its three decisions when something is wired up to take them.
fn plan_block_card(
    id: ElementId,
    plan_id: &str,
    items: &[String],
    sections: &[PlanSection],
    state: PlanState,
    folds: &Folds,
) -> AnyElement {
    // The fold stores plan steps as `String`; the card wants `SharedString`,
    // so this is the one conversion left, and it is the caller's now rather
    // than the component's (finding `library-hotpaths-8`).
    let items: Vec<SharedString> = items.iter().map(|item| SharedString::from(item.clone())).collect();
    let mut card = plan_card(id, &items).sections(sections.to_vec()).state(state);
    if let Some(handler) = folds.plan.clone() {
        let act = |action: PlanAction| {
            let handler = handler.clone();
            let plan_id = plan_id.to_owned();
            move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
                handler(plan_id.clone(), action, window, cx)
            }
        };
        card = card
            .on_accept(act(PlanAction::Accept))
            .on_edit(act(PlanAction::Refine))
            .on_reject(act(PlanAction::Reject));
    }
    card.into_any_element()
}

/// A failed turn. The retry resends the failed turn's own input, so it is
/// only offered when the app still has that text: a button that would send an
/// empty prompt is worse than no button.
fn error_block_card(
    id: ElementId,
    turn_id: &str,
    title: &str,
    detail: &str,
    retryable: bool,
    folds: &Folds,
) -> AnyElement {
    let mut card = error_card(id, title.to_owned(), detail.to_owned());
    if retryable {
        if let Some(cards) = &folds.cards {
            if cards.retryable_turns.contains(turn_id) {
                let retry = cards.retry.clone();
                let turn_id = turn_id.to_owned();
                card = card.on_retry(move |_, window, cx| retry(turn_id.clone(), window, cx));
            }
        }
    }
    card.into_any_element()
}

/// The long-running objective, with whatever work it has named.
fn goal_block_card(
    id: ElementId,
    objective: &str,
    status: &str,
    percent_complete: Option<f32>,
    current_work: Option<&str>,
    next_work: Option<&str>,
) -> AnyElement {
    let mut card = goal_card(id, objective.to_owned(), status.to_owned()).percent(percent_complete);
    if let Some(work) = current_work {
        card = card.current_work(work.to_owned());
    }
    if let Some(work) = next_work {
        card = card.next_work(work.to_owned());
    }
    card.into_any_element()
}

/// One tool invocation as its card: the body the lone `Block::ToolCall` would
/// have shown, with the same fold toggle and full-output fetch keyed by the
/// call's own id (so grouped calls behave like lone ones).
fn tool_call_card(key: &str, id: ElementId, call: &ToolCall, folds: &Folds) -> AnyElement {
    let block_id = call.id.clone();
    let full = folds.full_output.get(&block_id);
    let mut body = call.body.clone();
    // X2: an opened big diff is bounded — the display copy is cut to the
    // first hunk's first rows plus a count, while the stored block keeps
    // the whole diff. No new element is added here (the count rides an
    // existing diff row the library already draws), so no new role/label
    // is owed; the card toggle keeps its own.
    if let ToolBody::Edit { diff } = &body {
        let mut cut = display_diff(diff, DIFF_DISPLAY_CAP);
        cut.path = display_path(&cut.path, &folds.workspace_root, &home_dir());
        body = ToolBody::Edit { diff: cut };
    }
    // A fetched full output replaces the truncated visible text on the
    // server's result only (D4): the fold never changes, the card just
    // renders what the fetch returned.
    if let (ToolBody::Shell { output_lines, .. }, Some(full)) = (&mut body, full) {
        if let FullOutputState::Ready { lines, capped } = &full.state {
            *output_lines = lines.clone();
            if *capped {
                output_lines.push(crate::full_output::CAPPED_MARKER.to_owned());
            }
        }
    }
    // The card's own action slot: its fold row already emits Unfold,
    // so on a truncated card that intent is "Show full output" and
    // fetches; everywhere else every intent still just toggles, as
    // before. The row's label stays the library's ("N more lines") —
    // the library owns the card's text and this app does not change
    // it.
    let (fetchable, idle) = full
        .map(|full| (full.fetchable, matches!(full.state, FullOutputState::Idle)))
        .unwrap_or((false, false));
    let show = folds.show_full_output.clone();
    // D51's terminal card: while the tab lives the body mirrors the
    // block's last lines from the frame's snapshot; once the tab is gone
    // (or under `--replay`, where the snapshot is empty) the card keeps
    // the folded result text. The mirror flag decides the live pill —
    // the tab still runs — while the fold's own flag covers the rest.
    if is_terminal_card(call) {
        if let ToolBody::Shell { output_lines, live, .. } = &mut body {
            if let Some(mirror) = folds.terminal_mirror.get(&call.id).cloned() {
                *output_lines = mirror.lines;
                *live = mirror.live;
            }
        }
    }
    // D51's "Open terminal" on terminal cards, D49's "Run in terminal" on
    // every other shell card: a terminal command already ran in the dock,
    // so its card opens the dock rather than offering a rerun.
    let terminal = is_terminal_card(call);
    let open_tab = if terminal { terminal_tab_id(&call.target) } else { None };
    let run_command = shell_run_command(call);
    let mut actions = Vec::new();
    if terminal {
        actions.push(ToolCardAction::new(OPEN_IN_TERMINAL_ACTION_ID, OPEN_IN_TERMINAL_LABEL));
    } else if run_command.is_some() {
        actions.push(
            ToolCardAction::new(RUN_IN_TERMINAL_ACTION_ID, RUN_IN_TERMINAL_LABEL)
                .icon(IconName::Play),
        );
    }
    // X2: big Write/Edit cards start closed (the toggle still persists
    // through `folds`), and a shell header names only the command's first
    // line — the block keeps the full target, so run-in-terminal and the
    // tab lookup below still see the whole command. Z6b: a file header
    // names the workspace-relative path; the open/reveal below still see
    // the whole target.
    let mut card = tool_card(
        id,
        call.verb.clone(),
        display_target(call, &folds.workspace_root, &home_dir()),
        call.status,
        body,
    )
        .duration_ms(call.duration_ms)
        .open(folds.open(key, default_open(call)));
    if !actions.is_empty() {
        card = card.actions(actions.clone());
    }
    let run = folds.terminal_run.clone();
    let open = folds.open_terminal.clone();
    card
        .on_intent({
            let toggle = folds.toggle.clone();
            let key = key.to_owned();
            // B12: the open-in-pane fold row on search/MCP cards shows the
            // whole text in the right pane's doc view; without wiring it
            // keeps the toggle.
            let pane = tool_pane_text(call).zip(folds.open_full_text.clone());
            move |intent, window, cx| match intent {
                ToolCardIntent::OpenInPane => match &pane {
                    Some(((title, text), open)) => open(title.clone(), text.clone(), window, cx),
                    None => toggle(key.clone(), window, cx),
                },
                ToolCardIntent::Unfold if fetchable && idle => {
                    if let Some(show) = &show {
                        show(block_id.clone(), window, cx);
                    } else {
                        toggle(key.clone(), window, cx);
                    }
                }
                // "Open terminal" opens the dock on the card's tab; a
                // ⌥-click run pastes without Enter, a plain click sends
                // it. Anything that is not either action still toggles.
                ToolCardIntent::Action(index) => match (&open, &open_tab) {
                    (Some(open), _) if action_is_open(&actions, index) => {
                        open(open_tab.clone(), window, cx);
                    }
                    _ => match (&run, &run_command) {
                        (Some(run), Some(command)) if action_is_run(&actions, index) => {
                            if let Some(request) = RunRequest::new(
                                command.clone(),
                                send_enter_for_alt(window.modifiers().alt),
                            ) {
                                run(request, window, cx);
                            }
                        }
                        _ => toggle(key.clone(), window, cx),
                    },
                },
                _ => toggle(key.clone(), window, cx),
            }
        })
        .into_any_element()
}

/// The collapsed row a settled question leaves behind.
///
/// MSP settles a prompt six ways and only one of them is an answer, so the row
/// says which: an empty `Answer` after a cancel would otherwise read as
/// "Answered:" with nothing after it.
fn settled_row(id: ElementId, options: &[aui_protocol::QuestionOption], answer: &Answer) -> AnyElement {
    let chips: Vec<SharedString> = answer
        .selected
        .iter()
        .filter_map(|i| options.get(*i))
        .map(|o| SharedString::from(o.label.clone()))
        .collect();
    let outcome = match (chips.is_empty(), &answer.other) {
        // A settlement with free text and no chosen option is a clarification:
        // the person wrote instead of picking.
        (true, Some(text)) => QuestionOutcome::Clarified(SharedString::from(text.clone())),
        (true, None) => QuestionOutcome::Skipped,
        (false, _) => QuestionOutcome::Answered,
    };
    let mut chips = chips;
    chips.extend(answer.other.clone().filter(|_| outcome == QuestionOutcome::Answered).map(SharedString::from));
    answered_row(id, chips).outcome(outcome).into_any_element()
}

/// A hairline marker row, tinted only where the marker carries a warning.
///
/// The tint is the design rules' one exception: status colour carries meaning,
/// so a retry and a hole in the transcript are warning-coloured and everything
/// else is the muted line iconography every other marker uses.
fn marker(id: ElementId, kind: &MarkerKind, text: &str, folds: &Folds, cx: &mut App) -> AnyElement {
    use aui_tokens::ActiveAui;
    let p = cx.aui().colors;
    let row = marker_row(id);
    match kind {
        MarkerKind::SessionStarted => row.glyph(IconName::Play, None).text(text.to_owned()),
        MarkerKind::ContextCompacted => row.glyph(IconName::Layout, None).text(text.to_owned()),
        // The fold's own text already ends in the mode's label, so the row does
        // not name it twice; the emphasis goes on the label inside the text.
        MarkerKind::PermissionModeChanged { mode } => {
            let head = text.strip_suffix(mode.label()).unwrap_or(text);
            row.glyph(IconName::Shield, None).text(head.to_owned()).strong(mode.label())
        }
        MarkerKind::TurnCancelled => row.glyph(IconName::X, None).text("Turn interrupted").strong(text.to_owned()),
        MarkerKind::TurnRetracted => row.glyph(IconName::X, None).text("Prompt retracted"),
        MarkerKind::RetryScheduled => row.glyph(IconName::Refresh, Some(p.warning)).text(text.to_owned()),
        // Two rows share this marker: the promise a `view/gap` makes, and the
        // withdrawal of it when the backfill gave up (finding
        // `client-adapter-7`). The promise's text names a raw cursor, which is
        // not for a reader; the withdrawal's names the reason, which is.
        MarkerKind::ViewGap if text.starts_with(muse_adapter::GAP_ABORT_PREFIX) => row
            .glyph(IconName::X, Some(p.warning))
            .text("Backfill did not finish, so events may be missing above")
            .strong(text.trim_start_matches(muse_adapter::GAP_ABORT_PREFIX).to_owned()),
        MarkerKind::ViewGap => row
            .glyph(IconName::Shield, Some(p.warning))
            .text("Some events were missed while disconnected"),
        // The fold knows the source session's id and nothing else; the sidebar
        // knows what it is called, so the title is joined in here.
        MarkerKind::ForkedFrom => {
            let source = text.strip_prefix("Forked from ").unwrap_or(text);
            let label = folds.titles.get(source).cloned().unwrap_or_else(|| id_group(source));
            row.glyph(IconName::Git, None).text("Forked from ").strong(label)
        }
        // A handoff destination's origin marker: the quiet top line naming
        // where the session came from, with a link back to the source.
        // Without the back-link (a replayed transcript) the plain row
        // still says what happened.
        MarkerKind::HandOff { .. } => {
            let row = row.glyph(IconName::ArrowRight, None).text(text.to_owned());
            match &folds.handoff_back {
                Some(back) => {
                    let (source, open) = (back.source.clone(), back.open.clone());
                    row.link("Open the source session", move |_, window, cx| {
                        open(source.clone(), window, cx)
                    })
                }
                None => row,
            }
        }
    }
    .into_any_element()
}

/// The first group of a uuid, which is what a session with no title is called
/// everywhere else in this app.
fn id_group(id: &str) -> String {
    id.split('-').next().unwrap_or(id).to_owned()
}

/// `12.4 s`, `1 m 12 s` — the library's own formatting, so every duration in
/// the app reads the same.
pub fn elapsed(ms: u64) -> SharedString {
    aui::transcript::format_duration(ms)
}

/// The empty transcript's measure: the design bounds the transcript column,
/// composer included, and the suggestion chips sit centred inside it. Kept
/// equal to the centre pane's `TRANSCRIPT_MEASURE` (which lives next to the
/// rows it binds) rather than reaching across for it.
pub(crate) const EMPTY_STATE_MEASURE: f32 = 880.0;

/// The empty transcript: what a brand-new session shows before the first turn.
///
/// `display` is the project's display name — a rename changes it without
/// touching the folder, so the caller resolves it rather than this function
/// deriving a folder name. `provider` is the session's own provider: the
/// subtitle names whoever the session runs on, never a hardcoded Muse.
pub fn empty_state(
    provider: crate::providers::ProviderId,
    display: &str,
    hero: Option<AnyElement>,
    on_pick: Option<PickSuggestion>,
    cx: &mut App,
) -> AnyElement {
    use aui_tokens::{ActiveAui, AuiStyled};
    let p = cx.aui().colors;
    let mut column = v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap(px(scale::SP_3))
        .children(hero)
        .child(div().text_role(aui_tokens::TextRole::Title).text_color(p.ink_2).child("New session"))
        .child(div().ui(scale::FS_12).text_color(p.ink_3).child(provider.hero_subtitle(display)));
    // Three ways in, for a person looking at a blank page. They are prompts
    // about the workspace itself, so none of them assumes a project this is
    // not — and picking one only fills the composer, it never sends.
    if let Some(on_pick) = on_pick {
        // Under the deterministic flag the chips draw settled rather than
        // sparkling in: a capture is a static composition.
        let chips = aui::composer::suggestion_chips("empty-suggestions", SUGGESTIONS.iter().map(|s| (*s).into()).collect());
        let chips = if crate::clock::deterministic() { chips.at_rest() } else { chips };
        // Centred under the title, inside the measure: the chips element is
        // full-width and left-aligned, so the outer row centres the capped
        // box and the inner row centres the chips inside it.
        let chips = chips.on_pick(move |index, window, cx| on_pick(index, window, cx));
        let chips = div()
            .w_full()
            .flex()
            .justify_center()
            .child(div().flex().justify_center().max_w(px(EMPTY_STATE_MEASURE)).child(chips));
        column = column.child(chips);
    }
    column.into_any_element()
}

/// A suggestion chip was picked: the caller puts its text in the composer.
pub type PickSuggestion = std::rc::Rc<dyn Fn(usize, &mut gpui::Window, &mut App)>;

/// The text of the chip at `index`, for the caller that has to put it in the
/// composer.
pub fn suggestion(index: usize) -> Option<&'static str> {
    SUGGESTIONS.get(index).copied()
}

/// The three chips a fresh session offers.
///
/// Short, about the workspace rather than about a project Baaz has not
/// looked at, and each one is a thing a person genuinely opens a session for.
const SUGGESTIONS: [&str; 3] = [
    "What is in this workspace?",
    "Explain how this project is laid out",
    "Find the entry point and walk me through it",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(blocks: Vec<Block>, reasoning_tokens: u64) -> Turn {
        Turn::Assistant {
            id: "turn-1".to_owned(),
            blocks,
            meta: TurnMeta {
                model: "muse-spark-1.3-contributor".to_owned(),
                duration_ms: 17_339,
                tokens_in: 58_527,
                tokens_out: 625,
                reasoning_tokens,
                cost_usd: 0.0,
                ..TurnMeta::default()
            },
            timestamp: None,
        }
    }

    fn text_block() -> Block {
        Block::Text { text: "done".to_owned(), streaming: false }
    }

    fn thinking_block() -> Block {
        Block::Thinking {
            text: "hmm".to_owned(),
            elapsed_ms: 0,
            summary: None,
            state: ThinkingState::Done,
        }
    }

    #[test]
    fn display_path_without_a_workspace_still_abbreviates_home() {
        // Review finding: an empty root made every path "inside".
        assert_eq!(display_path("/Users/ada/notes/a.md", "", "/Users/ada"), "~/notes/a.md");
        assert_eq!(display_path("/Users/ada/notes/a.md", "/", "/Users/ada"), "~/notes/a.md");
        assert_eq!(display_path("/etc/hosts", "", "/Users/ada"), "/etc/hosts");
    }

    #[test]
    fn billed_reasoning_with_no_thinking_card_is_silent() {
        assert_eq!(silent_reasoning(&assistant(vec![text_block()], 419)), Some(419));
    }

    #[test]
    fn no_billed_reasoning_is_not_silent() {
        assert_eq!(silent_reasoning(&assistant(vec![text_block()], 0)), None);
    }

    #[test]
    fn a_visible_thinking_card_speaks_for_itself() {
        assert_eq!(
            silent_reasoning(&assistant(vec![thinking_block(), text_block()], 419)),
            None
        );
    }

    /// One list row per block, plus the silent footer, plus the bubble: the
    /// virtual list's item count is the sum of these over the cached turns.
    #[test]
    fn rows_are_blocks_and_the_silent_footer() {
        let user = Turn::User { id: "u".to_owned(), text: "hi".to_owned(), attachments: vec![], mentions: vec![], timestamp: None };
        assert_eq!(turn_rows(&user), 1);
        assert_eq!(turn_rows(&assistant(vec![text_block(), text_block(), text_block()], 0)), 3);
        // Reasoning tokens with no reasoning block: the footer row is added.
        assert_eq!(turn_rows(&assistant(vec![text_block()], 419)), 2);
    }

    #[test]
    fn a_user_turn_never_thinks_silently() {
        let turn = Turn::User {
            id: "user-1".to_owned(),
            text: "hi".to_owned(),
            attachments: Vec::new(),
            mentions: Vec::new(),
            timestamp: None,
        };
        assert_eq!(silent_reasoning(&turn), None);
    }

    #[test]
    fn the_silent_cell_names_the_footer_count() {
        assert_eq!(silent_reasoning_text(419), "419 reasoning");
    }

    #[test]
    fn turn_ages_are_just_now_under_a_minute() {
        let now = 1_800_000_000_000;
        assert_eq!(relative_words(now, now), "just now");
        assert_eq!(relative_words(now - 59_000, now), "just now");
        // A stamp from the future reads as now, never negative.
        assert_eq!(relative_words(now + 60_000, now), "just now");
    }

    #[test]
    fn turn_ages_count_minutes_then_hours() {
        let now = 1_800_000_000_000;
        assert_eq!(relative_words(now - 60_000, now), "1 minute ago");
        assert_eq!(relative_words(now - 59 * 60_000, now), "59 minutes ago");
        assert_eq!(relative_words(now - 3_600_000, now), "1 hour ago");
        assert_eq!(relative_words(now - 23 * 3_600_000, now), "23 hours ago");
    }

    #[test]
    fn turn_ages_say_yesterday_for_the_second_day() {
        let now = 1_800_000_000_000;
        assert_eq!(relative_words(now - 86_400_000, now), "yesterday");
        assert_eq!(relative_words(now - 47 * 3_600_000, now), "yesterday");
    }

    #[test]
    fn turn_ages_fall_back_to_the_calendar_date() {
        use chrono::{Datelike, TimeZone};
        let now = chrono::Local::now().timestamp_millis().max(0) as u64;
        let sent = now - 5 * 86_400_000;
        let date = chrono::Local.timestamp_millis_opt(sent as i64).single().expect("representable");
        let expected = if date.year() == chrono::Local::now().year() {
            date.format("%b %-d").to_string()
        } else {
            date.format("%b %-d, %Y").to_string()
        };
        assert_eq!(relative_words(sent, now), expected);
    }

    #[test]
    fn an_untimed_turn_has_no_age_caption() {
        assert_eq!(turn_age(None, 1_800_000_000_000), None);
        assert_eq!(turn_age(Some(1_800_000_000_000), 1_800_000_000_000).as_deref(), Some("just now"));
    }

    #[test]
    fn the_silent_footer_keeps_the_library_cells() {
        let Turn::Assistant { meta, .. } = assistant(vec![text_block()], 419) else {
            unreachable!("test helper builds an assistant turn")
        };
        assert_eq!(
            silent_footer_items(&meta, 419),
            vec![
                "muse-spark-1.3-contributor".to_owned(),
                "17.3 s".to_owned(),
                "59.2k tokens".to_owned(),
                "419 reasoning".to_owned(),
            ]
        );
    }

    /// Q2: a Claude Code wire id never reaches the footer raw — the
    /// footer reads the humanised label, while other lanes' ids pass
    /// through untouched.
    #[test]
    fn the_footer_humanises_a_claude_code_wire_id() {
        assert_eq!(footer_model_label("claude-haiku-4-5-20251001"), "Haiku 4.5");
        assert_eq!(footer_model_label("claude-opus-5[1m]"), "Opus 5 · 1M");
        assert_eq!(
            footer_model_label("muse-spark-1.3-contributor"),
            "muse-spark-1.3-contributor"
        );
        let meta = TurnMeta {
            model: "claude-haiku-4-5-20251001".to_owned(),
            ..TurnMeta::default()
        };
        assert!(
            silent_footer_items(&meta, 0).contains(&"Haiku 4.5".to_owned()),
            "the silent footer reads the label, drew {:?}",
            silent_footer_items(&meta, 0)
        );
    }

    fn shell_call(verb: &str, target: &str) -> ToolCall {
        ToolCall {
            id: "call-1".to_owned(),
            kind: ToolKind::Shell,
            verb: verb.to_owned(),
            target: target.to_owned(),
            status: aui_protocol::ToolStatus::Success,
            duration_ms: None,
            body: ToolBody::Shell { output_lines: vec!["ok".to_owned()], exit_code: Some(0), live: false },
            diff_stat: None,
        }
    }

    /// X2: an all-addition diff with `lines` rows, shaped like the folds'
    /// Write cards (Codex `fileChange` adds, Claude Code Write content).
    fn write_call(lines: usize) -> ToolCall {
        let diff = Diff {
            path: "index.html".to_owned(),
            hunks: vec![Hunk {
                header: format!("@@ -0,0 +1,{lines} @@"),
                lines: (1..=lines)
                    .map(|n| DiffLine {
                        kind: DiffKind::Add,
                        old_no: None,
                        new_no: Some(n as u32),
                        text: format!("<p>line {n}</p>"),
                    })
                    .collect(),
            }],
            added: lines as u32,
            removed: 0,
        };
        ToolCall {
            id: "call-write".to_owned(),
            kind: ToolKind::Write,
            verb: "Wrote".to_owned(),
            target: "index.html".to_owned(),
            status: aui_protocol::ToolStatus::Success,
            duration_ms: None,
            body: ToolBody::Edit { diff },
            diff_stat: None,
        }
    }

    /// The diff out of a test edit card.
    fn edit_diff(call: &ToolCall) -> Diff {
        match &call.body {
            ToolBody::Edit { diff } => diff.clone(),
            _ => panic!("test helper builds an edit card"),
        }
    }

    /// X2: the owner's 233-line HTML write starts closed — the wall of
    /// numbered HTML that prompted this task.
    #[test]
    fn a_233_line_write_starts_closed() {
        assert!(!default_open(&write_call(233)));
    }

    /// X2: a 5-line edit keeps today's open default, and so does the
    /// boundary itself — only *more than* 12 changed lines folds.
    #[test]
    fn small_edits_start_open() {
        let mut edit = write_call(5);
        edit.kind = ToolKind::Edit;
        edit.verb = "Edited".to_owned();
        assert!(default_open(&edit));
        assert!(default_open(&write_call(12)));
        assert!(!default_open(&write_call(13)));
    }

    /// X2: context rows are not changes — a big context with a small edit
    /// still starts open.
    #[test]
    fn context_rows_do_not_fold_a_card() {
        let mut call = write_call(5);
        if let ToolBody::Edit { diff } = &mut call.body {
            for line in &mut diff.hunks[0].lines {
                line.kind = DiffKind::Context;
            }
        }
        assert!(default_open(&call));
    }

    /// X2: non-edit cards always keep the open default.
    #[test]
    fn shell_cards_start_open() {
        assert!(default_open(&shell_call("Ran", "npm test")));
    }

    /// B8b: the session's edited files in edit order — two Edit blocks plus
    /// a grouped edit, with reads, shell calls and blanks filtered out and
    /// repeats keeping their first position.
    #[test]
    fn session_edits_list_edited_files_in_edit_order() {
        fn edit_call(target: &str) -> ToolCall {
            let mut call = write_call(3);
            call.kind = ToolKind::Edit;
            call.verb = "Edited".to_owned();
            call.target = target.to_owned();
            call
        }
        let mut read = write_call(3);
        read.kind = ToolKind::Read;
        read.target = "notes.md".to_owned();
        let mut blank = write_call(3);
        blank.kind = ToolKind::Edit;
        blank.target = "   ".to_owned();
        let repeat = edit_call("src/b.rs");
        let turns = vec![
            assistant(
                vec![
                    Block::ToolCall {
                        id: "c-read".to_owned(),
                        kind: read.kind.clone(),
                        verb: read.verb.clone(),
                        target: read.target.clone(),
                        status: aui_protocol::ToolStatus::Success,
                        duration_ms: None,
                        body: ToolBody::None,
                        diff_stat: None,
                    },
                    Block::ToolCall {
                        id: "c-1".to_owned(),
                        kind: ToolKind::Edit,
                        verb: "Edited".to_owned(),
                        target: "src/b.rs".to_owned(),
                        status: aui_protocol::ToolStatus::Success,
                        duration_ms: None,
                        body: ToolBody::None,
                        diff_stat: None,
                    },
                ],
                0,
            ),
            assistant(
                vec![
                    Block::ToolGroup {
                        calls: vec![edit_call("src/a.rs"), shell_call("Ran", "cargo test")],
                        summary: "edited".to_owned(),
                        state: ActivityState::Done,
                    },
                    Block::ToolCall {
                        id: "c-blank".to_owned(),
                        kind: ToolKind::Edit,
                        verb: "Edited".to_owned(),
                        target: "   ".to_owned(),
                        status: aui_protocol::ToolStatus::Success,
                        duration_ms: None,
                        body: ToolBody::None,
                        diff_stat: None,
                    },
                    Block::ToolCall {
                        id: "c-repeat".to_owned(),
                        kind: repeat.kind.clone(),
                        verb: repeat.verb.clone(),
                        target: repeat.target.clone(),
                        status: aui_protocol::ToolStatus::Success,
                        duration_ms: None,
                        body: ToolBody::None,
                        diff_stat: None,
                    },
                ],
                0,
            ),
        ];
        assert_eq!(session_edited_paths(&turns), vec!["src/b.rs".to_owned(), "src/a.rs".to_owned()]);
        assert!(session_edited_paths(&[]).is_empty());
    }

    #[test]
    fn the_edits_cache_key_is_turns_then_last_blocks() {
        // B8c: the Changes pane caches the edit list on (turn count, last
        // turn's block count) — O(1), no block scan.
        assert_eq!(edited_paths_cache_key(&[]), (0, 0));
        let one = vec![assistant(vec![text_block()], 0)];
        assert_eq!(edited_paths_cache_key(&one), (1, 1));
        let two = vec![assistant(vec![text_block()], 0), assistant(vec![text_block(), text_block()], 0)];
        assert_eq!(edited_paths_cache_key(&two), (2, 2));
        // A user turn last carries no blocks.
        let user = Turn::User {
            id: "u-1".to_owned(),
            text: "hi".to_owned(),
            attachments: Vec::new(),
            mentions: Vec::new(),
            timestamp: None,
        };
        let mixed = vec![assistant(vec![text_block(), text_block(), text_block()], 0), user];
        assert_eq!(edited_paths_cache_key(&mixed), (2, 0));
    }

    /// X2: an opened big diff draws the first 40 rows of the first hunk
    /// plus a final `… N more lines` row counting its dropped rows.
    #[test]
    fn opened_big_diffs_show_forty_rows_and_a_count() {
        let shown = display_diff(&edit_diff(&write_call(233)), DIFF_DISPLAY_CAP);
        assert_eq!(shown.hunks.len(), 1);
        assert_eq!(shown.hunks[0].lines.len(), DIFF_DISPLAY_CAP + 1);
        assert_eq!(shown.hunks[0].lines[DIFF_DISPLAY_CAP].text, "… 193 more lines");
        // The chip counts still describe the whole change.
        assert_eq!((shown.added, shown.removed), (233, 0));
    }

    /// X2 review: every hunk after the first survives — a small edit in
    /// two places keeps both, and a long first hunk is cut without taking
    /// the later ones with it.
    #[test]
    fn later_hunks_are_never_dropped() {
        let mut two = edit_diff(&write_call(3));
        let mut second = two.hunks[0].clone();
        second.header = "@@ -20,0 +20,3 @@".to_owned();
        two.hunks.push(second.clone());
        assert_eq!(display_diff(&two, DIFF_DISPLAY_CAP), two);

        let mut long = edit_diff(&write_call(100));
        long.hunks.push(second);
        let shown = display_diff(&long, DIFF_DISPLAY_CAP);
        assert_eq!(shown.hunks.len(), 2);
        assert_eq!(shown.hunks[0].lines[DIFF_DISPLAY_CAP].text, "… 60 more lines");
        assert_eq!(shown.hunks[1].lines.len(), 3);
    }

    /// X2: short diffs pass through untouched — no count row appended.
    #[test]
    fn short_diffs_pass_through_unchanged() {
        let diff = edit_diff(&write_call(5));
        assert_eq!(display_diff(&diff, DIFF_DISPLAY_CAP), diff);
    }

    /// X2: a heredoc command's header is its first line only — the whole
    /// script is no longer the card's title.
    #[test]
    fn a_heredoc_header_is_its_first_line_only() {
        let command = "cat > /tmp/x.html <<'EOF'\n<html>\n<body>\nEOF";
        assert_eq!(
            display_target(&shell_call("Ran", command), "/ws", "/home/u"),
            "cat > /tmp/x.html <<'EOF'"
        );
    }

    /// X2: a long first line caps at ~160 chars with `…`.
    #[test]
    fn long_commands_cap_with_an_ellipsis() {
        let command = "x".repeat(200);
        let shown = display_target(&shell_call("Ran", &command), "/ws", "/home/u");
        assert_eq!(shown.chars().count(), COMMAND_TARGET_CHARS + 1);
        assert!(shown.ends_with('…'));
    }

    /// X2: a short single-line command shows whole, and relative
    /// non-shell targets are never touched.
    #[test]
    fn short_commands_and_non_shell_targets_show_whole() {
        assert_eq!(display_target(&shell_call("Ran", "npm test"), "/ws", "/home/u"), "npm test");
        let write = write_call(5);
        assert_eq!(display_target(&write, "/ws", "/home/u"), "index.html");
    }

    /// Z6b: an absolute path inside the workspace reads relative to it.
    #[test]
    fn inside_the_workspace_reads_relative() {
        assert_eq!(
            display_path("/ws/crates/baaz/src/app.rs", "/ws", "/home/u"),
            "crates/baaz/src/app.rs"
        );
    }

    /// Z6b: an absolute path outside the workspace stays absolute, with
    /// `$HOME` abbreviated to `~`.
    #[test]
    fn outside_the_workspace_stays_absolute_with_home_abbreviated() {
        assert_eq!(display_path("/home/u/other/file.rs", "/ws", "/home/u"), "~/other/file.rs");
        assert_eq!(display_path("/etc/passwd", "/ws", "/home/u"), "/etc/passwd");
        assert_eq!(display_path("/home/u", "/ws", "/home/u"), "~");
    }

    /// Z6b: the workspace root itself has no relative remainder, and a
    /// sibling directory sharing a string prefix is outside.
    #[test]
    fn the_root_itself_and_a_shared_prefix_sibling_stay_absolute() {
        assert_eq!(display_path("/a/harness", "/a/harness", "/home/u"), "/a/harness");
        assert_eq!(
            display_path("/a/harness-int/x.rs", "/a/harness", "/home/u"),
            "/a/harness-int/x.rs"
        );
    }

    /// Z6b: file cards show the shortened path while the block keeps the
    /// full target for reveals.
    #[test]
    fn file_card_headers_shorten_but_keep_the_full_target() {
        let mut read = shell_call("Read", "/ws/src/main.rs");
        read.kind = ToolKind::Read;
        assert_eq!(display_target(&read, "/ws", "/home/u"), "src/main.rs");
        assert_eq!(read.target, "/ws/src/main.rs");
        let mut read = shell_call("Read", "/home/u/notes/todo.md");
        read.kind = ToolKind::Read;
        assert_eq!(display_target(&read, "/ws", "/home/u"), "~/notes/todo.md");
    }

    /// D49: a shell tool card carries the card's command text — Muse's
    /// shell tool and the `!` userShell fold to the same shape, so one
    /// wiring covers both.
    #[test]
    fn shell_cards_carry_their_command_text() {
        assert_eq!(
            shell_run_command(&shell_call("Ran", "npm test -- --watch")),
            Some("npm test -- --watch".to_owned())
        );
        assert_eq!(
            shell_run_command(&shell_call("$", "git status -sb")),
            Some("git status -sb".to_owned())
        );
    }

    #[test]
    fn non_shell_cards_offer_no_run() {
        let mut read = shell_call("Read", "src/main.rs");
        read.kind = ToolKind::Read;
        read.body = ToolBody::Read { lines: 3 };
        assert_eq!(shell_run_command(&read), None);
        // A search that fell back to a shell body is not a command: its
        // target is a pattern, so it must not offer to run.
        let mut search = shell_call("Searched", "pattern");
        search.kind = ToolKind::Search;
        assert_eq!(shell_run_command(&search), None);
        assert_eq!(shell_run_command(&shell_call("Ran", "   ")), None);
    }

    fn run_actions() -> Vec<ToolCardAction> {
        vec![
            ToolCardAction::new("copy-output", "Copy output"),
            ToolCardAction::new(RUN_IN_TERMINAL_ACTION_ID, RUN_IN_TERMINAL_LABEL),
        ]
    }

    /// D49: the `Action(usize)` payload is an index, so the run resolves by
    /// id at that index — a card carrying more than one action still maps
    /// back to the right command.
    #[test]
    fn the_action_index_maps_back_to_the_run() {
        let actions = run_actions();
        assert!(!action_is_run(&actions, 0));
        assert!(action_is_run(&actions, 1));
        assert!(!action_is_run(&actions, 2));
        assert!(!action_is_run(&[], 0));
    }

    /// D49: `runnable_command`'s output is what gets run — a `bash` block
    /// whole, a `console` block's prompt lines with prompts stripped, an
    /// untagged `$ `-prefixed block the same way.
    #[test]
    fn runnable_fences_run_what_runnable_command_returns() {
        assert_eq!(
            code_run_command(Some("bash"), "npm test -- --watch"),
            Some("npm test -- --watch".to_owned())
        );
        assert_eq!(
            code_run_command(Some("console"), "$ npm test\n42 passing\n$ npm run lint"),
            Some("npm test\nnpm run lint".to_owned())
        );
        assert_eq!(
            code_run_command(None, "$ git status\n$ git diff --stat"),
            Some("git status\ngit diff --stat".to_owned())
        );
    }

    /// D49: a `rust` block offers no button, and neither does an untagged
    /// block mixing prompts with output.
    #[test]
    fn unrunnable_fences_offer_no_run() {
        assert_eq!(code_run_command(Some("rust"), "let x = 1;"), None);
        assert_eq!(code_run_command(Some("python"), "print('hi')"), None);
        assert_eq!(code_run_command(None, "$ git status\nOn branch main"), None);
        assert_eq!(code_run_command(None, "let x = 1;"), None);
    }

    /// D49: multi-line blocks paste whole — the shell runs the lines in
    /// order — so the offer keeps every line.
    #[test]
    fn multi_line_blocks_run_whole() {
        let command = "npm test -- --watch\nnpm run lint";
        assert_eq!(code_run_command(Some("bash"), command), Some(command.to_owned()));
        let blocks = runnable_blocks(&format!("Try it:\n\n```bash\n{command}\n```\n"));
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].command, command);
        assert_eq!(blocks[0].label, "npm test -- --watch");
    }

    /// The run row offers one button per runnable fence, in fence order,
    /// and nothing for prose or unrunnable fences.
    #[test]
    fn the_run_row_lists_exactly_the_runnable_fences() {
        let text = "First:\n\n```bash\ngit status\n```\n\nThen:\n\n```rust\nlet x = 1;\n```\n\nFinally:\n\n```console\n$ npm test\nok\n```\n";
        let blocks = runnable_blocks(text);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].command, "git status");
        assert_eq!(blocks[1].command, "npm test");
        assert!(runnable_blocks("Just prose, no fences.").is_empty());
        assert!(runnable_blocks("```rust\nlet x = 1;\n```\n").is_empty());
    }

    /// W7b: the pending question names the host and fits the tool. The
    /// muse lane keeps its own title; a provider session asks in the
    /// provider's name with the tool's verb. Drop a verb arm and its
    /// line here names the fallback instead.
    #[test]
    fn approval_titles_name_the_provider_and_fit_the_tool() {
        assert_eq!(approval_title(None, "Bash"), APPROVAL_TITLE);
        assert_eq!(approval_title(None, "Write"), APPROVAL_TITLE);
        assert_eq!(approval_title(Some("Claude Code"), "Bash"), "Allow Claude Code to run this command?");
        assert_eq!(approval_title(Some("Claude Code"), "Write"), "Allow Claude Code to write this file?");
        assert_eq!(approval_title(Some("Claude Code"), "Edit"), "Allow Claude Code to edit this file?");
        assert_eq!(approval_title(Some("Claude Code"), "MultiEdit"), "Allow Claude Code to edit this file?");
        assert_eq!(approval_title(Some("Claude Code"), "WebFetch"), "Allow Claude Code to fetch this URL?");
        assert_eq!(approval_title(Some("Codex"), "Bash"), "Allow Codex to run this command?");
        assert_eq!(approval_title(Some("Codex"), "Edit"), "Allow Codex to edit this file?");
        assert_eq!(
            approval_title(Some("Codex"), "Permissions"),
            "Allow Codex to change permissions?"
        );
        assert_eq!(
            approval_title(Some("Claude Code"), "mcp__server__tool"),
            "Allow Claude Code to use mcp__server__tool?"
        );
        assert_eq!(
            approval_title(Some("Claude Code"), "mcp__baaz__terminal_run"),
            "Allow Claude Code to run this command in the terminal?"
        );
        assert_eq!(approval_title(None, "terminal_run"), APPROVAL_TITLE);
    }

    /// D51: a terminal card is a shell card with the terminal verb — only
    /// it gets "Open terminal" and the mirror. Its tab parses off the
    /// target's strict `· t<digits>` suffix; anything else is no tab
    /// rather than a wrong one.
    #[test]
    fn terminal_cards_are_detected_and_their_tabs_parsed() {
        use aui_protocol::{ToolBody, ToolStatus};
        let shell = |verb: &str, target: &str| ToolCall {
            id: "i-1".into(),
            kind: ToolKind::Shell,
            verb: verb.into(),
            target: target.into(),
            status: ToolStatus::Success,
            duration_ms: None,
            body: ToolBody::Shell { output_lines: Vec::new(), exit_code: Some(0), live: false },
            diff_stat: None,
        };
        let ran = shell(TERMINAL_RAN_VERB, "echo hi · t1");
        assert!(is_terminal_card(&ran));
        assert_eq!(terminal_tab_id(&ran.target), Some("t1".to_owned()));
        assert_eq!(terminal_command(&ran.target), "echo hi");
        assert_eq!(shell_run_command(&ran), None, "no rerun on a terminal card");
        let running = shell(TERMINAL_RUNNING_VERB, "sleep 30");
        assert!(is_terminal_card(&running));
        assert_eq!(terminal_tab_id(&running.target), None, "no tab yet");
        assert_eq!(terminal_command(&running.target), "sleep 30");
        // A command that merely ends in something tab-like is not a tab:
        // the suffix must be exactly `· t<digits>`.
        let tricky = shell(TERMINAL_RAN_VERB, "echo · t1x");
        assert_eq!(terminal_tab_id(&tricky.target), None);
        assert_eq!(terminal_command(&tricky.target), "echo · t1x");
        let plain = shell("Ran", "echo hi · t1");
        assert!(!is_terminal_card(&plain), "an ordinary shell card keeps Run in terminal");
        assert_eq!(shell_run_command(&plain), Some("echo hi · t1".to_owned()));
        let read = ToolCall { kind: ToolKind::Read, ..shell(TERMINAL_RAN_VERB, "x") };
        assert!(!is_terminal_card(&read), "the verb alone never marks the card");
    }

    fn quiet_folds() -> Folds {
        Folds {
            toggled: Rc::new(HashSet::new()),
            toggle: Rc::new(|_: String, _: &mut Window, _: &mut App| {}),
            plan: None,
            cards: None,
            titles: Rc::new(HashMap::new()),
            approval_host: None,
            full_output: Rc::new(HashMap::new()),
            show_full_output: None,
            at_rest: true,
            now_ms: 0,
            copied: Rc::new(HashSet::new()),
            link: None,
            assistant_action: None,
            user_action: None,
            span_held: Rc::new(HashMap::new()),
            span_event: None,
            tool_group: None,
            terminal_run: None,
            open_terminal: None,
            terminal_mirror: Rc::new(HashMap::new()),
            terminal_live: false,
            skill_scopes: Rc::new(HashMap::new()),
            open_skill: None,
            handoff: None,
            handoff_back: None,
            workspace_root: "/ws".to_owned(),
            fold_finished_turns: true,
            open_full_text: None,
            open_turn_diff: None,
            tool_starts: Rc::new(HashMap::new()),
        }
    }

    fn thinking_id() -> ElementId {
        ElementId::from(SharedString::from("thought"))
    }

    #[test]
    fn skill_loads_are_named_but_other_reads_are_not() {
        use aui_protocol::{ToolBody, ToolStatus};
        let load = ToolCall {
            id: "i-1".into(),
            kind: ToolKind::Read,
            verb: "Loaded skill".into(),
            target: "bundled:plan".into(),
            status: ToolStatus::Success,
            duration_ms: None,
            body: ToolBody::None,
            diff_stat: None,
        };
        assert_eq!(skill_load_name(&load), Some("bundled:plan"));
        let read = ToolCall { verb: "Read".into(), target: "notes.md".into(), ..load.clone() };
        assert_eq!(skill_load_name(&read), None);
        let blank = ToolCall { target: "  ".into(), ..load.clone() };
        assert_eq!(skill_load_name(&blank), None);
        assert_eq!(skill_load_text("plan", Some("built-in")), "Loaded skill `plan` · built-in");
        assert_eq!(skill_load_text("plan", None), "Loaded skill `plan`");
        assert_eq!(skill_load_text("plan", Some(" ")), "Loaded skill `plan`");
    }

    /// W7b: a trace with no text earns no card — redacted (signature-only)
    /// thinking must never become an empty "Thought for 0.0 s" shell. A
    /// real trace still cards. Unwire the guard and the empty lines card.
    #[test]
    fn empty_thinking_earns_no_card() {
        let folds = quiet_folds();
        assert!(
            thinking_card(thinking_id(), "t:0", "", 0, None, ThinkingState::Done, &folds).is_none(),
            "empty text cards nothing"
        );
        assert!(
            thinking_card(thinking_id(), "t:0", "   \n  ", 0, None, ThinkingState::Done, &folds)
                .is_none(),
            "whitespace-only text cards nothing"
        );
        assert!(
            thinking_card(thinking_id(), "t:0", "hmm", 0, None, ThinkingState::Done, &folds).is_some(),
            "a real trace still cards"
        );
    }

    /// B12: a read call, a search call, a shell call and a write, shaped
    /// like the three lanes' folds mint them.
    fn read_call(target: &str) -> Block {
        Block::ToolCall {
            id: format!("read:{target}"),
            kind: ToolKind::Read,
            verb: "Read".to_owned(),
            target: target.to_owned(),
            status: aui_protocol::ToolStatus::Success,
            duration_ms: Some(120),
            body: ToolBody::Read { lines: 42 },
            diff_stat: None,
        }
    }

    fn search_call(target: &str) -> Block {
        Block::ToolCall {
            id: format!("search:{target}"),
            kind: ToolKind::Search,
            verb: "Searched".to_owned(),
            target: target.to_owned(),
            status: aui_protocol::ToolStatus::Success,
            duration_ms: Some(80),
            body: ToolBody::Search { hits: Vec::new() },
            diff_stat: None,
        }
    }

    fn failed_call() -> Block {
        let mut call = shell_call("Ran", "exit 1");
        call.status = aui_protocol::ToolStatus::Error;
        Block::tool_call(call)
    }

    fn mixed_turn() -> Turn {
        Turn::Assistant {
            id: "turn-fold".to_owned(),
            blocks: vec![
                Block::Text { text: "Let me check the layout.".to_owned(), streaming: false },
                read_call("crates/baaz/src/app.rs"),
                search_call("fold_finished_turns"),
                Block::tool_call(shell_call("Ran", "cargo test")),
                Block::ToolCall {
                    id: "call-write".to_owned(),
                    kind: ToolKind::Write,
                    verb: "Wrote".to_owned(),
                    target: "index.html".to_owned(),
                    status: aui_protocol::ToolStatus::Success,
                    duration_ms: Some(40),
                    body: ToolBody::Edit { diff: edit_diff(&write_call(5)) },
                    diff_stat: Some(aui_protocol::DiffStat { added: 5, removed: 0, files: 1 }),
                },
                thinking_block(),
                Block::Text { text: "Done — folded rows are quiet.".to_owned(), streaming: false },
            ],
            meta: TurnMeta {
                model: "m".to_owned(),
                duration_ms: 134_000,
                tokens_in: 0,
                tokens_out: 0,
                reasoning_tokens: 0,
                cost_usd: 0.0,
                ..TurnMeta::default()
            },
            timestamp: None,
        }
    }

    fn empty_toggled() -> HashSet<String> {
        HashSet::new()
    }

    /// B12: a mixed turn folds to one header row plus the final answer:
    /// tool cards, thinking and interim prose hide; the summary counts
    /// the work and the chip carries the diff.
    #[test]
    fn fold_row_counts_and_summary_for_a_mixed_turn() {
        let turn = mixed_turn();
        let plan = fold_plan(&turn).expect("a turn with tool activity folds");
        assert_eq!((plan.reads, plan.edits, plan.commands), (1, 1, 1));
        assert_eq!(plan.elapsed_ms, 134_000);
        assert_eq!((plan.diff_added, plan.diff_removed), (5, 0));
        // Interim prose (0), four tool cards (1-4), thinking (5) fold;
        // the final answer (6) stays out.
        assert_eq!(plan.folded, vec![0, 1, 2, 3, 4, 5]);
        assert!(plan.visible.is_empty());
        assert_eq!(plan.answer, Some(6));
        // Closed: header + answer. Open: header + all seven blocks.
        assert_eq!(turn_mapped_rows(&turn, true, &empty_toggled(), true), 2);
        let mut open = HashSet::new();
        open.insert(fold_key("turn-fold"));
        assert_eq!(turn_mapped_rows(&turn, true, &open, true), 8);
        // Off: every block keeps its row.
        assert_eq!(turn_mapped_rows(&turn, true, &empty_toggled(), false), 7);
    }

    /// B12: the final answer is outside the fold: the closed mapping
    /// addresses the header then the answer block, in that order.
    #[test]
    fn final_answer_stays_outside_the_fold() {
        let turn = mixed_turn();
        let toggled = empty_toggled();
        assert!(matches!(turn_slot_at(&turn, true, &toggled, true, 0), Some(TurnSlot::FoldHeader)));
        assert!(matches!(
            turn_slot_at(&turn, true, &toggled, true, 1),
            Some(TurnSlot::Block(6))
        ));
    }

    /// B12: kept-visible kinds stay out of the fold: a failed call, a
    /// denied approval, an answered question, a todo list and a marker.
    #[test]
    fn kept_visible_kinds_stay_visible() {
        assert!(!approval_folds(&ApprovalState::Denied));
        assert!(!approval_folds(&ApprovalState::Pending));
        assert!(approval_folds(&ApprovalState::AllowedOnce { exit_code: 0, duration_ms: 9 }));
        let turn = Turn::Assistant {
            id: "turn-kept".to_owned(),
            blocks: vec![
                read_call("a.rs"),
                failed_call(),
                Block::Question {
                    id: "q".to_owned(),
                    header: String::new(),
                    prompt: "Which?".to_owned(),
                    subtitle: String::new(),
                    options: Vec::new(),
                    multi: false,
                    allow_other: false,
                    answer: Some(Answer { selected: vec![], other: None }),
                    timeout_ms: None,
                },
                Block::Todo {
                    items: vec![aui_protocol::TodoItem {
                        label: "x".to_owned(),
                        state: aui_protocol::TodoState::Done,
                        elapsed_ms: None,
                    }],
                },
                Block::Marker { kind: MarkerKind::ContextCompacted, text: "compacted".to_owned() },
                Block::Text { text: "answer".to_owned(), streaming: false },
            ],
            meta: TurnMeta::default(),
            timestamp: None,
        };
        let plan = fold_plan(&turn).expect("tool activity folds");
        assert_eq!(plan.folded, vec![0]);
        assert_eq!(plan.visible, vec![1, 2, 3, 4]);
        assert_eq!(plan.answer, Some(5));
        assert_eq!(turn_mapped_rows(&turn, true, &empty_toggled(), true), 6);
    }

    /// B12: a turn with no tool activity gets no fold row — a fold must
    /// hide something.
    #[test]
    fn a_turn_with_no_tool_activity_gets_no_fold() {
        let turn = assistant(vec![text_block(), thinking_block()], 0);
        assert!(fold_plan(&turn).is_none());
        let user = Turn::User {
            id: "u".to_owned(),
            text: "hi".to_owned(),
            attachments: vec![],
            mentions: vec![],
            timestamp: None,
        };
        assert!(fold_plan(&user).is_none());
    }

    /// B12: a live run of quiet calls collapses to one row naming the
    /// current call with "+N earlier"; an edit breaks the run. B12fix: a
    /// lone quiet call earns the live row too, and the elapsed shown is
    /// the run's own measured time.
    #[test]
    fn live_run_collapses_to_one_row_with_the_right_verb_target() {
        let turn = Turn::Assistant {
            id: "turn-live".to_owned(),
            blocks: vec![
                read_call("a.rs"),
                read_call("b.rs"),
                search_call("fold"),
                Block::Text { text: "Meanwhile…".to_owned(), streaming: false },
                read_call("c.rs"),
            ],
            meta: TurnMeta::default(),
            timestamp: None,
        };
        let runs = live_runs(&turn, 0, &HashMap::new());
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].blocks, vec![0, 1, 2]);
        // Q1: every member finished, so the row reads past tense —
        // never a "Searching" spinner for settled work.
        assert_eq!(runs[0].verb, "Searched");
        assert!(!runs[0].running);
        assert_eq!(runs[0].target, "fold");
        assert_eq!(runs[0].earlier, 2);
        // The elapsed is the run's own measured time (120 + 120 + 80),
        // not the turn's age.
        assert_eq!(runs[0].elapsed_ms, 320);
        // The lone trailing read earns its own live row.
        assert_eq!(runs[1].blocks, vec![4]);
        assert_eq!(runs[1].earlier, 0);
        assert_eq!(runs[1].elapsed_ms, 120);
        // Five blocks collapse to run + prose + run: 3 rows.
        assert_eq!(turn_mapped_rows(&turn, false, &empty_toggled(), true), 3);
        assert!(matches!(
            turn_slot_at(&turn, false, &empty_toggled(), true, 0),
            Some(TurnSlot::LiveRun(_))
        ));
        assert!(matches!(
            turn_slot_at(&turn, false, &empty_toggled(), true, 2),
            Some(TurnSlot::LiveRun(_))
        ));
        // An edit keeps its own card and never joins a run.
        let mut edit = write_call(3);
        edit.kind = ToolKind::Edit;
        let turn = Turn::Assistant {
            id: "turn-live-edit".to_owned(),
            blocks: vec![read_call("a.rs"), read_call("b.rs"), Block::ToolCall {
                id: "e".to_owned(),
                kind: ToolKind::Edit,
                verb: "Edited".to_owned(),
                target: "a.rs".to_owned(),
                status: aui_protocol::ToolStatus::Success,
                duration_ms: None,
                body: ToolBody::Edit { diff: edit_diff(&edit) },
                diff_stat: None,
            }],
            meta: TurnMeta::default(),
            timestamp: None,
        };
        let runs = live_runs(&turn, 0, &HashMap::new());
        assert_eq!(runs.len(), 1);
        assert_eq!(turn_mapped_rows(&turn, false, &empty_toggled(), true), 2);
    }

    /// B12: generic cards start collapsed — the chevron has something to
    /// open — and open when the person toggles them.
    #[test]
    fn generic_cards_start_collapsed_with_a_working_toggle() {
        let folds = quiet_folds();
        assert!(!folds.open("turn-1:9", false));
        let mut toggled = HashSet::new();
        toggled.insert("turn-1:9".to_owned());
        let folds = Folds { toggled: Rc::new(toggled), ..quiet_folds() };
        assert!(folds.open("turn-1:9", false));
    }

    /// B12: the fold carries the turn's real elapsed time, not a frozen
    /// zero — the header reads what the turn cost.
    #[test]
    fn thinking_and_fold_elapsed_are_real() {
        let turn = mixed_turn();
        let plan = fold_plan(&turn).expect("folds");
        assert_eq!(plan.elapsed_ms, 134_000);
        assert_eq!(aui::transcript::turn_fold_elapsed(134_000), "2m 14s");
    }

    /// B12: the open-in-pane affordance covers search hits, MCP results
    /// and shell output — never reads or bare headers.
    #[test]
    fn open_in_pane_covers_search_mcp_and_shell() {
        let mut search = shell_call("Searched", "fold");
        search.kind = ToolKind::Search;
        search.body = ToolBody::Search {
            hits: vec![aui_protocol::SearchHit {
                path: "a.rs".to_owned(),
                line: 3,
                snippet: "fold".to_owned(),
            }],
        };
        let (title, text) = tool_pane_text(&search).expect("pane text");
        assert!(title.contains("fold"));
        assert!(text.contains("a.rs:3"));
        let mut read = shell_call("Read", "a.rs");
        read.kind = ToolKind::Read;
        read.body = ToolBody::Read { lines: 3 };
        assert!(tool_pane_text(&read).is_none());
    }

    /// B12fix: a turn ending on a tool has no separate answer — the last
    /// Text stays visible in its original position, before the ending
    /// failure, and the closed mapping keeps that order.
    #[test]
    fn no_answer_when_the_turn_ends_on_a_tool() {
        let turn = Turn::Assistant {
            id: "turn-tail-tool".to_owned(),
            blocks: vec![
                Block::Text { text: "Trying a build.".to_owned(), streaming: false },
                read_call("a.rs"),
                failed_call(),
            ],
            meta: TurnMeta::default(),
            timestamp: None,
        };
        let plan = fold_plan(&turn).expect("tool activity folds");
        assert_eq!(plan.answer, None);
        assert_eq!(plan.folded, vec![1]);
        assert_eq!(plan.visible, vec![0, 2]);
        // Header, then the prose where the turn put it, then the failure.
        assert_eq!(turn_mapped_rows(&turn, true, &empty_toggled(), true), 3);
        assert!(matches!(
            turn_slot_at(&turn, true, &empty_toggled(), true, 1),
            Some(TurnSlot::Block(0))
        ));
        assert!(matches!(
            turn_slot_at(&turn, true, &empty_toggled(), true, 2),
            Some(TurnSlot::Block(2))
        ));
    }

    /// B12fix: a recovered failure folds — the retry is the visible
    /// outcome — while an unrecovered one stays visible.
    #[test]
    fn recovered_failures_fold_while_unrecovered_stay() {
        let mut retry_failed = shell_call("Ran", "make");
        retry_failed.status = aui_protocol::ToolStatus::Error;
        let retry_ok = shell_call("Ran", "make");
        let mut other_failed = shell_call("Ran", "lint");
        other_failed.status = aui_protocol::ToolStatus::Error;
        let turn = Turn::Assistant {
            id: "turn-recovered".to_owned(),
            blocks: vec![
                Block::tool_call(retry_failed),
                Block::tool_call(retry_ok),
                Block::tool_call(other_failed),
                Block::Text { text: "Lint still red.".to_owned(), streaming: false },
            ],
            meta: TurnMeta::default(),
            timestamp: None,
        };
        let plan = fold_plan(&turn).expect("tool activity folds");
        assert_eq!(plan.folded, vec![0, 1]);
        assert_eq!(plan.visible, vec![2]);
        assert_eq!(plan.answer, Some(3));
    }

    /// B12fix: a ToolGroup with one failure folds behind its header —
    /// keeping it out would show every successful call beside the one
    /// that failed.
    #[test]
    fn a_tool_group_with_an_unrecovered_failure_stays_visible() {
        let mut failed = shell_call("Ran", "make");
        failed.status = aui_protocol::ToolStatus::Error;
        let turn = Turn::Assistant {
            id: "turn-group-fail".to_owned(),
            blocks: vec![
                Block::ToolGroup {
                    calls: vec![shell_call("Ran", "setup"), failed, shell_call("Ran", "teardown")],
                    summary: "Ran 3 commands".into(),
                    state: aui_protocol::ActivityState::Failed,
                },
                Block::Text { text: "The middle step failed.".to_owned(), streaming: false },
            ],
            meta: TurnMeta::default(),
            timestamp: None,
        };
        let plan = fold_plan(&turn).expect("tool activity folds");
        assert!(plan.folded.is_empty(), "the failed group is not hidden in the fold");
        assert_eq!(plan.visible, vec![0]);
        assert_eq!(plan.answer, Some(1));
    }

    /// B12fix: an empty Thinking block takes no row — closed, open, live
    /// or settled.
    #[test]
    fn an_empty_thinking_block_takes_no_row() {
        let empty = Block::Thinking {
            text: "  ".to_owned(),
            elapsed_ms: 0,
            summary: None,
            state: ThinkingState::Done,
        };
        let turn = Turn::Assistant {
            id: "turn-empty-think".to_owned(),
            blocks: vec![
                read_call("a.rs"),
                empty,
                Block::Text { text: "Done.".to_owned(), streaming: false },
            ],
            meta: TurnMeta::default(),
            timestamp: None,
        };
        let plan = fold_plan(&turn).expect("tool activity folds");
        assert_eq!(plan.folded, vec![0]);
        assert!(plan.visible.is_empty());
        assert_eq!(plan.answer, Some(2));
        // Closed: header + answer. Open: header + read + answer — the
        // empty trace is addressed nowhere.
        assert_eq!(turn_mapped_rows(&turn, true, &empty_toggled(), true), 2);
        let mut open = HashSet::new();
        open.insert(fold_key("turn-empty-think"));
        assert_eq!(turn_mapped_rows(&turn, true, &open, true), 3);
        assert!(matches!(
            turn_slot_at(&turn, true, &open, true, 1),
            Some(TurnSlot::Block(0))
        ));
        assert!(matches!(
            turn_slot_at(&turn, true, &open, true, 2),
            Some(TurnSlot::Block(2))
        ));
        // Live: the empty trace breaks no run and takes no row.
        assert_eq!(turn_mapped_rows(&turn, false, &empty_toggled(), true), 2);
    }

    /// B12fix: finished fast commands join the live run; only a command
    /// still RUNNING past 2 s keeps its own card.
    #[test]
    fn finished_fast_commands_join_the_run() {
        let mut fast = shell_call("Ran", "make");
        fast.duration_ms = Some(900);
        let mut slow_running = shell_call("Run", "sleep 30");
        slow_running.status = aui_protocol::ToolStatus::Running;
        slow_running.duration_ms = Some(5_000);
        let turn = Turn::Assistant {
            id: "turn-shell-join".to_owned(),
            blocks: vec![read_call("a.rs"), Block::tool_call(fast), Block::tool_call(slow_running)],
            meta: TurnMeta::default(),
            timestamp: None,
        };
        let runs = live_runs(&turn, 0, &HashMap::new());
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].blocks, vec![0, 1]);
        // Q1: the finished members read past tense with their measured
        // total (120 + 900) — a finished shell never reads "Running".
        assert_eq!(runs[0].verb, "Ran");
        assert!(!runs[0].running);
        assert_eq!(runs[0].elapsed_ms, 1020);
        assert_eq!(turn_mapped_rows(&turn, false, &empty_toggled(), true), 2);
    }

    /// Q1: a finished shell call never produces a "Running" verb or a
    /// live (spinning) row: it reads past tense with its measured total.
    #[test]
    fn a_finished_shell_call_never_reads_running() {
        let mut done = shell_call("Ran", "cargo test");
        done.duration_ms = Some(7_000);
        let turn = Turn::Assistant {
            id: "turn-shell-done".to_owned(),
            blocks: vec![Block::tool_call(done)],
            meta: TurnMeta::default(),
            timestamp: None,
        };
        let runs = live_runs(&turn, 5_000, &HashMap::new());
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].verb, "Ran");
        assert_eq!(runs[0].target, "cargo test");
        assert!(!runs[0].running, "nothing in flight: no spinner, no tick");
        assert_eq!(runs[0].elapsed_ms, 7_000);
    }

    /// Q1b: a run with anything still in flight ticks from the run's
    /// FIRST member start — never measured durations plus the turn age
    /// (a 60 s-old turn with a finished 10 s read and a running grep
    /// read 70 s before this fix), and never more than the turn's age.
    #[test]
    fn a_live_run_ticks_from_its_first_member_start() {
        let mut read = read_call("a.rs");
        if let Block::ToolCall { id, duration_ms, .. } = &mut read {
            *id = "read-1".to_owned();
            *duration_ms = Some(10_000);
        } else {
            panic!("a read card");
        }
        let mut flying = search_call("pattern");
        if let Block::ToolCall { id, status, duration_ms, .. } = &mut flying {
            *id = "grep-1".to_owned();
            *status = aui_protocol::ToolStatus::Running;
            *duration_ms = None;
        } else {
            panic!("a search card");
        }
        let turn = Turn::Assistant {
            id: "turn-run-start".to_owned(),
            blocks: vec![read, flying],
            meta: TurnMeta::default(),
            timestamp: Some(1_000),
        };
        // The turn is 60 s old; the run's first member started at 2 s.
        let mut starts = HashMap::new();
        starts.insert("read-1".to_owned(), 2_000);
        let runs = live_runs(&turn, 61_000, &starts);
        assert_eq!(runs.len(), 1);
        assert!(runs[0].running, "in flight: the row spins and ticks");
        assert_eq!(runs[0].elapsed_ms, 59_000, "now minus the first member start");
        assert!(runs[0].elapsed_ms <= 60_000, "never more than the turn's age");
        // No start known: the turn timestamp is the fallback — the turn's
        // age, still never the measured total on top of it.
        let runs = live_runs(&turn, 61_000, &HashMap::new());
        assert_eq!(runs[0].elapsed_ms, 60_000);
    }

    /// Q1b: an approved command renders exactly one visible item — the
    /// approval card — in the live slots: no second run row and no
    /// separate tool card either way round the wire orders them. The
    /// terminal face's backticked shape covers the call all the same.
    #[test]
    fn an_approved_command_is_only_its_approval_card() {
        fn approval_card(id: &str, tool: &str, command: &str) -> Block {
            Block::approval(
                id,
                tool,
                command,
                "run the suite",
                "/tmp",
                Vec::new(),
                aui_protocol::ApprovalScope::ThisCommand,
                ApprovalState::Pending,
                None,
            )
        }
        fn flying(target: &str) -> Block {
            let mut call = shell_call("Run", target);
            call.id = format!("call:{target}");
            call.status = aui_protocol::ToolStatus::Running;
            call.duration_ms = None;
            Block::tool_call(call)
        }
        // Approval first, then the call it gates.
        let turn = Turn::Assistant {
            id: "turn-approved".to_owned(),
            blocks: vec![approval_card("req-1", "Bash", "cargo test"), flying("cargo test")],
            meta: TurnMeta::default(),
            timestamp: Some(1_000),
        };
        assert!(live_runs(&turn, 8_000, &HashMap::new()).is_empty(), "the card is the row");
        assert_eq!(turn_mapped_rows(&turn, false, &empty_toggled(), true), 1);
        assert!(matches!(
            turn_slot_at(&turn, false, &empty_toggled(), true, 0),
            Some(TurnSlot::Block(0))
        ));
        // Live wire order: the `tool_use` cards before its `can_use_tool`.
        let turn = Turn::Assistant {
            id: "turn-approved-live-order".to_owned(),
            blocks: vec![flying("cargo test"), approval_card("req-1", "Bash", "cargo test")],
            meta: TurnMeta::default(),
            timestamp: Some(1_000),
        };
        assert!(live_runs(&turn, 8_000, &HashMap::new()).is_empty(), "the card is the row");
        assert_eq!(turn_mapped_rows(&turn, false, &empty_toggled(), true), 1);
        assert!(matches!(
            turn_slot_at(&turn, false, &empty_toggled(), true, 0),
            Some(TurnSlot::Block(1))
        ));
        // The terminal face wraps the same command in backticks; it
        // covers the call all the same.
        let mut terminal = shell_call("Running in terminal", "echo hi");
        terminal.id = "call:echo hi".to_owned();
        terminal.status = aui_protocol::ToolStatus::Running;
        terminal.duration_ms = None;
        let turn = Turn::Assistant {
            id: "turn-approved-terminal".to_owned(),
            blocks: vec![
                approval_card("req-2", "mcp__baaz__terminal_run", "Run in terminal · `echo hi`"),
                Block::tool_call(terminal),
            ],
            meta: TurnMeta::default(),
            timestamp: Some(1_000),
        };
        assert!(live_runs(&turn, 8_000, &HashMap::new()).is_empty(), "the terminal card is the row");
        assert_eq!(turn_mapped_rows(&turn, false, &empty_toggled(), true), 1);
    }

    /// Q1b: approval coverage is 1:1 — one approval hides one call. A
    /// later re-run of the same command matches no unconsumed approval,
    /// so it keeps its own row, either way round the wire orders them.
    /// A hidden call splits no run: quiet calls around one read as one run.
    #[test]
    fn a_second_run_of_the_same_command_still_shows() {
        fn approval_card(id: &str, command: &str) -> Block {
            Block::approval(
                id,
                "Bash",
                command,
                "run the suite",
                "/tmp",
                Vec::new(),
                aui_protocol::ApprovalScope::ThisCommand,
                ApprovalState::Pending,
                None,
            )
        }
        fn flying(id: &str, target: &str) -> Block {
            let mut call = shell_call("Run", target);
            call.id = id.to_owned();
            call.status = aui_protocol::ToolStatus::Running;
            call.duration_ms = None;
            Block::tool_call(call)
        }
        // Live wire order: first run, its approval, then the re-run.
        let turn = Turn::Assistant {
            id: "turn-rerun".to_owned(),
            blocks: vec![flying("c-1", "cargo test"), approval_card("req-1", "cargo test"), flying("c-2", "cargo test")],
            meta: TurnMeta::default(),
            timestamp: Some(1_000),
        };
        if let Turn::Assistant { blocks, .. } = &turn {
            assert_eq!(covered_tool_blocks(blocks), HashSet::from([0]));
        } else {
            panic!("an assistant turn");
        }
        // The approval card stands, the re-run reads as its own live row.
        assert_eq!(turn_mapped_rows(&turn, false, &empty_toggled(), true), 2);
        assert!(matches!(
            turn_slot_at(&turn, false, &empty_toggled(), true, 0),
            Some(TurnSlot::Block(1))
        ));
        assert!(matches!(
            turn_slot_at(&turn, false, &empty_toggled(), true, 1),
            Some(TurnSlot::LiveRun(_))
        ));
        // Approval first: it covers the first matching call after it —
        // the re-run still shows.
        let turn = Turn::Assistant {
            id: "turn-rerun-approval-first".to_owned(),
            blocks: vec![approval_card("req-1", "cargo test"), flying("c-1", "cargo test"), flying("c-2", "cargo test")],
            meta: TurnMeta::default(),
            timestamp: Some(1_000),
        };
        if let Turn::Assistant { blocks, .. } = &turn {
            assert_eq!(covered_tool_blocks(blocks), HashSet::from([1]));
        } else {
            panic!("an assistant turn");
        }
        assert_eq!(turn_mapped_rows(&turn, false, &empty_toggled(), true), 2);
        // The hidden call splits no run: the quiet reads around one flow
        // as a single run past the trailing approval card.
        let turn = Turn::Assistant {
            id: "turn-hidden-no-split".to_owned(),
            blocks: vec![
                read_call("a.rs"),
                flying("c-1", "make"),
                search_call("fold"),
                approval_card("req-1", "make"),
            ],
            meta: TurnMeta::default(),
            timestamp: Some(1_000),
        };
        let runs = live_runs(&turn, 8_000, &HashMap::new());
        assert_eq!(runs.len(), 1, "one run flows around the hidden call");
        assert_eq!(runs[0].blocks, vec![0, 2]);
    }

    /// B12fix: Artifact tools read as a one-line summary, never the
    /// generic dump.
    #[test]
    fn artifact_tools_read_as_a_one_line_summary() {
        assert_eq!(artifact_summary_text("My Launch Post"), "Artifact · published My Launch Post");
        assert_eq!(
            artifact_summary_text("https://example.com/a\nsecond line"),
            "Artifact · published https://example.com/a"
        );
        assert_eq!(artifact_summary_text("   "), "Artifact · quickstart");
        assert_eq!(artifact_summary_text(""), "Artifact · quickstart");
    }

    /// B12fix: the cached plan agrees with a fresh walk and re-plans when
    /// a call settles in place (same block count, new failure).
    #[test]
    fn the_cached_fold_plan_agrees_and_notices_updates() {
        let turn = Turn::Assistant {
            id: "turn-cache-probe".to_owned(),
            blocks: vec![
                Block::tool_call(shell_call("Ran", "make")),
                Block::Text { text: "Green.".to_owned(), streaming: false },
            ],
            meta: TurnMeta::default(),
            timestamp: None,
        };
        let toggled = empty_toggled();
        let first = fold_plan_cached(&turn, true, &toggled).expect("folds");
        let fresh = fold_plan(&turn).expect("folds");
        assert_eq!(first.folded, fresh.folded);
        assert_eq!(first.visible, fresh.visible);
        assert_eq!(first.answer, fresh.answer);
        // The same call failing in place re-plans: it turns visible.
        let mut failed = shell_call("Ran", "make");
        failed.status = aui_protocol::ToolStatus::Error;
        let updated = Turn::Assistant {
            id: "turn-cache-probe".to_owned(),
            blocks: vec![
                Block::tool_call(failed),
                Block::Text { text: "Green.".to_owned(), streaming: false },
            ],
            meta: TurnMeta::default(),
            timestamp: None,
        };
        let second = fold_plan_cached(&updated, true, &toggled).expect("folds");
        assert_eq!(second.visible, vec![0]);
        assert!(second.folded.is_empty());
    }
}
