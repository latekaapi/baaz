//! `provider-claude-code` — the Claude Code CLI implementation of the
//! provider seam.
//!
//! Neutral [`Command`]s in, `claude` argv on a pipe, neutral [`Ack`]s and
//! [`ProviderEvent`]s out. Every wire spelling (frame shapes, flags, slug
//! transform) lives in this crate; nothing outside it names one.
//!
//! Honest limits (see the report): the gate reads five recorded files. This
//! adapter has never run against a live CLI in this task — no turn was ever
//! steered or interrupted, `--fork-session` and sub-agent forwarding were
//! read off `--help`, the `<cwd-slug>` transform was observed on one
//! directory, and no Baaz UI has ever rendered one of these frames.

#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

pub mod account;
pub mod argv;
pub mod auth_status;
pub mod caps;
pub mod child;
pub mod controls;
pub mod fold;
pub mod frame;
pub mod history;
pub mod home;
pub mod terminal;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use aui_protocol::{Delta, PermissionMode};
use crossbeam_channel::{unbounded, Receiver, Sender};
use provider::{
    Ack, CapabilitySet, Command, ConnectInfo, Handshake, ModelSummary, PendingApproval,
    ProviderAdapter, ProviderError, ProviderEvent, ProviderId, SessionSummary,
};

pub use caps::{capabilities, claude_version_supported, CLAUDE_VERSION_FLOOR};

use argv::SessionLaunch;
use child::RunningChild;
use controls::{ControlHub, PendingControl, PendingControlKind};
use fold::{ClaudeFold, UnknownControlRequest};
use frame::ApprovalRequest;

pub use controls::{
    CONTROL_CONFIRM_TIMEOUT, CONTROL_EFFORT_REJECTED_CARD, CONTROL_MODEL_REJECTED_CARD,
    CONTROL_MODE_FALLBACK_CARD, CONTROL_MODE_REJECTED_CARD,
};
pub use fold::CHILD_EXITED_CARD;

/// One claimed answer: the request is already out of its queue, so this
/// value is the only right to answer it. Exactly one claimant can hold
/// it, which is what keeps two concurrent decisions from writing two
/// `control_response` lines for one `request_id`.
#[derive(Debug)]
enum ClaimedApproval {
    /// A `can_use_tool` request plus its minted answer line.
    Known(ApprovalRequest, String),
    /// An unknown-subtype request plus its minted answer line.
    Unknown(UnknownControlRequest, String),
}

impl ClaimedApproval {
    /// The minted `control_response` line to write to the child.
    fn line(&self) -> &str {
        match self {
            ClaimedApproval::Known(_, line) | ClaimedApproval::Unknown(_, line) => line,
        }
    }
}

/// The Claude Code implementation: neutral commands in, one long-lived
/// `claude` child per session, neutral acks and events out.
///
/// The event pump is the same decode-and-fold the fixture tests exercise
/// ([`fold::pump_reader`]), sharing the fold under a mutex so `ReadAccount`
/// sees the live meter.
pub struct ClaudeCodeAdapter {
    program: String,
    tx: Sender<ProviderEvent>,
    rx: Receiver<ProviderEvent>,
    fold: Arc<Mutex<ClaudeFold>>,
    child: Mutex<Option<RunningChild>>,
    /// The held session id, shared with the [`ControlHub`] so a refusal
    /// banner scopes to the right session even when the pump confirms it.
    session_id: Arc<Mutex<Option<String>>>,
    /// The session's recorded model, shared with the [`ControlHub`]: the
    /// optimistic pick from here on, rolled back when the child refuses.
    model: Arc<Mutex<Option<String>>>,
    /// The effort level in effect on the running child, when one was set:
    /// raised with `apply_flag_settings{effortLevel}` (verified live
    /// 2026-09-28 — the next turn's session jsonl carries the level), or
    /// the `--effort` launch flag when the child spawned with one. `None`
    /// is Default: no override in effect. Clearing back to Default has no
    /// verified control-channel reset, so it keeps the `--resume`
    /// relaunch path (see [`Self::resume_launch_for_effort`]). Shared with
    /// the [`ControlHub`], which rolls it back when the child refuses.
    effort: Arc<Mutex<Option<String>>>,
    /// Whether the running child may switch into `bypassPermissions` over
    /// the control channel: only a child launched in it may (probed live
    /// 2026-10-16 — any other answers "the session was not launched with
    /// --dangerously-skip-permissions").
    bypass_capable: Mutex<bool>,
    /// A mode pick the running child cannot take live — Full access on a
    /// child not launched in it, or any way out of Read-only on a child
    /// launched in it (its `--settings` deny layer cannot be shed live):
    /// the next `SubmitInput` relaunches with `--resume` and the mode flag
    /// first, the way a cleared effort does. A later live pick clears it.
    mode_relaunch_due: Mutex<Option<PermissionMode>>,
    /// Where the running child's Read-only deny layer
    /// ([`argv::read_only_settings`]) came from, which decides how a pick
    /// away from Read-only sheds it.
    read_only_layer: Mutex<ReadOnlyLayer>,
    /// The confirmation hub: pending host control requests, matched by
    /// `request_id` when the child's answers arrive on the pump. The pump
    /// holds the same [`Arc`], so confirmation runs on arrival — never on
    /// the next dispatch.
    hub: Arc<ControlHub>,
    /// Host→child control `request_id` sequence (`baaz-ctl-…`), shared by
    /// `initialize`, `set_model` and `apply_flag_settings` alike.
    control_seq: Mutex<u64>,
    /// Every host→child control line addressed, in order — written to the
    /// child when one runs, recorded regardless so tests can read the
    /// bytes a spawn would carry.
    sent_control: Mutex<Vec<String>>,
    workspace: Mutex<Option<PathBuf>>,
    home_override: Option<PathBuf>,
    config_override: Mutex<Option<PathBuf>>,
    connected: Mutex<bool>,
    version: Mutex<Option<String>>,
    /// Where the terminal relay lives, set by the host before the session
    /// commands run: the bridge binary, the socket to point it at, and the
    /// directory (`<support_dir>/mcp`) its per-session config files land
    /// in. `None` means no relay: sessions spawn with no `--mcp-config`
    /// file (but still with `--strict-mcp-config` below).
    terminal: Mutex<Option<TerminalRelay>>,
    /// Whether the session may inherit the owner's own MCP servers and
    /// connectors: the Settings → Providers "Use my own MCP servers"
    /// switch (`Layout.use_own_mcp.claude_code`), set by the host before
    /// the session commands run. Off means `--strict-mcp-config` on every
    /// launch (the default); on means the flag is stripped while the
    /// bridge config still rides when a relay is set. Applies to sessions
    /// started after the change.
    use_own_mcp: Mutex<bool>,
}

/// The host→child control lines the adapter addresses, each answering
/// `{"subtype":"success"}` with its `request_id` (every shape probed live
/// 2026-09-28 against the installed CLI).
///
/// `initialize` restates the model catalog (`response.response.models[]`,
/// recorded in `fixtures/claude-code/permission.jsonl`); the fold keeps it
/// for `ListModels`.
pub fn initialize_control_request(request_id: &str) -> String {
    serde_json::json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {"subtype": "initialize"},
    })
    .to_string()
}

/// `set_model` switches the live child's model from the next turn, with no
/// relaunch: spawned on `haiku`, switched to `sonnet`, the next turn's
/// assistant message reported `claude-sonnet-5`.
pub fn set_model_control_request(request_id: &str, model: &str) -> String {
    serde_json::json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {"subtype": "set_model", "model": model},
    })
    .to_string()
}

/// `apply_flag_settings{effortLevel}` raises the live child's effort from
/// the next turn, with no relaunch: set to `high` on `sonnet`, the next
/// turn's session jsonl carried `"effort":"high"`. There is no verified
/// reset — clearing back to Default keeps the `--resume` relaunch path.
pub fn apply_effort_control_request(request_id: &str, level: &str) -> String {
    serde_json::json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {"subtype": "apply_flag_settings", "settings": {"effortLevel": level}},
    })
    .to_string()
}

/// `set_permission_mode` switches the live child's approval mode with no
/// relaunch: answered `{"mode": …}` and followed by a `system/status`
/// frame carrying `permissionMode` (probed live 2026-10-16, every mode but
/// `bypassPermissions` from a child not launched in it).
pub fn set_permission_mode_control_request(request_id: &str, mode: PermissionMode) -> String {
    serde_json::json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {"subtype": "set_permission_mode", "mode": argv::claude_permission_mode(mode)},
    })
    .to_string()
}

/// `apply_flag_settings` carrying the Read-only deny layer onto the live
/// child (`on`), or nulling it back off (probed 2026-10-16: the rules show
/// in `list_permission_rules` from the answer on, and are gone after the
/// clear, while the owner's own rules stand throughout).
pub fn apply_read_only_control_request(request_id: &str, on: bool) -> String {
    let settings = if on { argv::read_only_settings() } else { argv::read_only_settings_cleared() };
    serde_json::json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {"subtype": "apply_flag_settings", "settings": settings},
    })
    .to_string()
}

/// Where the running child's Read-only deny layer came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReadOnlyLayer {
    /// None in force.
    Off,
    /// Rode `--settings` at launch: only a relaunch sheds it.
    Launched,
    /// Applied live with `apply_flag_settings`: a live clear sheds it.
    Live,
}

/// Where the terminal relay lives: the bridge to spawn and the socket to
/// point it at, plus where its per-session MCP config files land.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalRelay {
    /// Absolute path of the `mcp-bridge` binary beside the running baaz
    /// binary (or inside the app bundle next to it).
    pub bridge: PathBuf,
    /// `<support_dir>/run/terminal-<pid>.sock`: what the bridge's
    /// `--socket` names.
    pub socket: PathBuf,
    /// `<support_dir>/mcp`: the directory each session's `--mcp-config`
    /// file is written into.
    pub config_dir: PathBuf,
}

impl ClaudeCodeAdapter {
    /// Hold the CLI `program` (usually `claude`). Spawning stays in
    /// [`ProviderAdapter::connect`] and the session commands; tests never
    /// spawn — every test runs offline against the checked-in fixtures.
    pub fn new(program: &str) -> Self {
        let (tx, rx) = unbounded();
        // The stream fold: live wall-clock readings for tool starts and
        // turn starts, so the transcript's live rows tick for real.
        // Stored history folds through a fresh replay fold instead
        // (see `stored_history`), keeping its filed zeros.
        let fold = Arc::new(Mutex::new({
            let mut fold = ClaudeFold::new();
            fold.set_live(true);
            fold
        }));
        let model = Arc::new(Mutex::new(None));
        let effort = Arc::new(Mutex::new(None));
        let session_id = Arc::new(Mutex::new(None));
        let hub = Arc::new(ControlHub::new(
            Arc::clone(&fold),
            tx.clone(),
            Arc::clone(&model),
            Arc::clone(&effort),
            Arc::clone(&session_id),
        ));
        Self {
            program: program.into(),
            tx,
            rx,
            fold,
            child: Mutex::new(None),
            session_id,
            model,
            effort,
            bypass_capable: Mutex::new(false),
            read_only_layer: Mutex::new(ReadOnlyLayer::Off),
            mode_relaunch_due: Mutex::new(None),
            hub,
            control_seq: Mutex::new(1),
            sent_control: Mutex::new(Vec::new()),
            workspace: Mutex::new(None),
            home_override: None,
            config_override: Mutex::new(None),
            connected: Mutex::new(false),
            version: Mutex::new(None),
            terminal: Mutex::new(None),
            use_own_mcp: Mutex::new(false),
        }
    }

    /// Point the terminal relay at the bridge, socket and config directory
    /// before the session commands run. Every open, resume, fork and
    /// effort-swap relaunch then writes its per-session MCP config and
    /// spawns with `--mcp-config <path> --strict-mcp-config` (strict
    /// unless [`Self::set_use_own_mcp`] opted the session into the
    /// owner's own servers).
    pub fn set_terminal_relay(&self, relay: TerminalRelay) {
        *self.terminal.lock().expect("terminal mutex") = Some(relay);
    }

    /// Let the session inherit the owner's own MCP servers and
    /// connectors: the Settings → Providers "Use my own MCP servers"
    /// switch. When on, launches strip `--strict-mcp-config` (the bridge
    /// config still rides when a relay is set). Off by default.
    pub fn set_use_own_mcp(&self, use_own: bool) {
        *self.use_own_mcp.lock().expect("use-own-mcp mutex") = use_own;
    }

    /// Apply the opt-in to a built launch: strip `--strict-mcp-config`
    /// when the switch is on, else leave the launch as built (strict on
    /// every launch — see [`argv`]).
    fn apply_use_own_mcp(&self, launch: SessionLaunch) -> SessionLaunch {
        if *self.use_own_mcp.lock().expect("use-own-mcp mutex") {
            argv::without_strict_mcp(launch)
        } else {
            launch
        }
    }

    /// The `--mcp-config` path for `session_id`, writing its file first —
    /// or `None` when no relay is set. A session without a relay spawns
    /// exactly today's argv.
    fn mcp_config_for(&self, session_id: &str) -> Result<Option<String>, ProviderError> {
        let Some(relay) = self.terminal.lock().expect("terminal mutex").clone() else {
            return Ok(None);
        };
        let path = terminal::write_mcp_config(&relay.config_dir, session_id, &relay.bridge, &relay.socket)
            .map_err(|error| ProviderError::Unavailable {
                reason: format!("could not write the terminal MCP config: {error}"),
            })?;
        Ok(Some(path.to_string_lossy().into_owned()))
    }

    /// The open launch for `request_id`: the caller-chosen session id plus
    /// the terminal relay's config file, when one is set — and
    /// `--strict-mcp-config` on every launch, relay or not (Z5), unless
    /// the opt-in stripped it. Pure apart from that one file write — no
    /// spawn — so tests drive this without a CLI.
    pub fn launch_for_open(
        &self,
        request_id: &str,
        workspace: Option<&str>,
        model: Option<&str>,
    ) -> Result<SessionLaunch, ProviderError> {
        let mcp = self.mcp_config_for(request_id)?;
        Ok(self.apply_use_own_mcp(argv::argv_for_open(
            request_id,
            workspace,
            model,
            mcp.as_deref(),
            None,
        )))
    }

    /// The resume launch for a stored `session_id`, with the relay's
    /// config file when one is set and strict unless opted in. No spawn —
    /// see [`Self::launch_for_open`].
    pub fn launch_for_resume(&self, session_id: &str) -> Result<SessionLaunch, ProviderError> {
        let mcp = self.mcp_config_for(session_id)?;
        Ok(self.apply_use_own_mcp(argv::argv_for_resume(session_id, None, mcp.as_deref(), None)))
    }

    /// The fork launch branching `session_id` into `request_id`, with the
    /// relay's config file (for the NEW session) when one is set and
    /// strict unless opted in. No spawn.
    pub fn launch_for_fork(
        &self,
        request_id: &str,
        session_id: &str,
    ) -> Result<SessionLaunch, ProviderError> {
        let mcp = self.mcp_config_for(request_id)?;
        Ok(self
            .apply_use_own_mcp(argv::argv_for_fork(request_id, session_id, None, mcp.as_deref(), None)))
    }

    /// Carry the relay onto an effort-swap `launch`: the swap relaunches
    /// with `--resume`, and the resumed child must see the same bridge the
    /// old one did. A launch that already carries `--mcp-config` (every
    /// relayed [`Self::launch_for_open`] one does) keeps its file — the
    /// file is written once per session, not once per swap — but still
    /// gains (or, opted in, loses) `--strict-mcp-config`, so a swap never
    /// silently widens or narrows what the session may see.
    fn with_mcp_config(&self, mut launch: SessionLaunch) -> Result<SessionLaunch, ProviderError> {
        let use_own = *self.use_own_mcp.lock().expect("use-own-mcp mutex");
        if !launch.argv.iter().any(|arg| arg == "--mcp-config") {
            if let Some(path) = self.mcp_config_for(&launch.session_id.clone())? {
                launch.argv.push("--mcp-config".into());
                launch.argv.push(path);
            }
        }
        let has_strict = launch.argv.iter().any(|arg| arg == "--strict-mcp-config");
        if use_own && has_strict {
            launch = argv::without_strict_mcp(launch);
        } else if !use_own && !has_strict {
            launch.argv.push("--strict-mcp-config".into());
        }
        Ok(launch)
    }

    /// Override `$HOME` for stored-history lookup (tests).
    pub fn with_home(mut self, home: PathBuf) -> Self {
        self.home_override = Some(home);
        self
    }

    /// Pin the Claude config dir for history lookup and child env
    /// (tests, and the host after ensuring the Baaz home). Wins over
    /// `$CLAUDE_CONFIG_DIR`, which wins over the defaults below.
    pub fn with_config_dir(self, dir: PathBuf) -> Self {
        self.set_config_dir(dir);
        self
    }

    /// Pin the Claude config dir after construction: the host ensures
    /// the Baaz-owned home and pins it here, so history and children
    /// resolve through it even when this process's own env names
    /// another config dir.
    pub fn set_config_dir(&self, dir: PathBuf) {
        *self.config_override.lock().expect("config mutex") = Some(dir);
    }

    /// Override the session workspace cwd for stored-history lookup
    /// (tests). Production sets this from `OpenSession.workspace` in
    /// [`Self::spawn_launch`]; it is what [`history::stored_transcript_path`]
    /// resolves and slugs.
    pub fn with_workspace(self, workspace: PathBuf) -> Self {
        *self.workspace.lock().expect("workspace mutex") = Some(workspace);
        self
    }

    /// The Claude config dir this adapter reads history through and
    /// spawns children with: the pinned dir first, then
    /// `$CLAUDE_CONFIG_DIR` when set non-empty, then the test stand-in's
    /// `.claude`, then the Baaz-owned home under this run's state dir
    /// (which honours `BAAZ_STATE_DIR`). Infallible: every branch names
    /// a dir, even when `HOME` is unset.
    fn resolved_config_dir(&self) -> PathBuf {
        if let Some(dir) = self.config_override.lock().expect("config mutex").clone() {
            return dir;
        }
        if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|dir| !dir.is_empty()) {
            return PathBuf::from(dir);
        }
        match &self.home_override {
            Some(home) => home.join(".claude"),
            None => crate::home::default_home(),
        }
    }

    /// The owner's real Claude root for a pre-consent legacy resume:
    /// the test stand-in's `.claude`, else `~/.claude` under the real
    /// `HOME`, else the Baaz-owned home (no owner home is knowable).
    fn owner_config_dir(&self) -> PathBuf {
        match &self.home_override {
            Some(owner) => crate::home::owner_config_dir(owner),
            None => std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|owner| crate::home::owner_config_dir(&owner))
                .unwrap_or_else(crate::home::default_home),
        }
    }

    /// Best-effort home upkeep before a spawn: re-run the absent-only
    /// owner links over the resolved Baaz home, so entries the owner
    /// created after adapter setup (`skills/`, `agents/`, …) are linked
    /// on the next spawn instead of waiting for a restart. Cheap stat
    /// checks only ([`provider::child_env::ensure_linked_dir`]); errors
    /// are ignored — the spawn below reports an unusable home honestly.
    /// Skipped for a legacy resume: that child reads the owner's home
    /// itself, and linking the owner onto itself would plant Baaz-home
    /// symlinks inside it.
    fn prepare_home(&self, legacy: bool) {
        if legacy {
            return;
        }
        let home = self.resolved_config_dir();
        let owner_root = self.owner_config_dir();
        let _ = provider::child_env::ensure_linked_dir(&home, &owner_root, crate::home::LINKED_ENTRIES);
    }

    /// Mint a host→child control `request_id`. One sequence for every
    /// subtype — the child joins answers by id, never by kind.
    fn next_control_id(&self) -> String {
        let mut seq = self.control_seq.lock().expect("control sequence mutex");
        let id = format!("baaz-ctl-{}", *seq);
        *seq += 1;
        id
    }

    /// Address one host→child control request: recorded always, written
    /// when a child runs, and TRACKED from the send until the child's
    /// answer confirms or refuses it (see [`ControlHub`]). With no child
    /// there is nothing to deliver to, so the line is recorded but not
    /// delivered — an admission the child never saw, matching
    /// `SelectModel`'s offline-capable contract — and still tracked, so a
    /// silence past the timeout refuses loudly rather than posing as
    /// applied. A delivery failure untracks at once: no answer can arrive
    /// for a line the child never took.
    fn send_control(&self, line: &str, pending: PendingControl) -> Result<(), ProviderError> {
        self.sent_control.lock().expect("sent control mutex").push(line.to_owned());
        let request_id = pending.request_id.clone();
        self.hub.track(pending);
        let delivered = match self.child.lock().expect("child mutex").as_mut() {
            Some(running) => running.send_line(line).map_err(|error| ProviderError::Unavailable {
                reason: format!("the session child is unreachable: {error}"),
            }),
            None => Ok(()),
        };
        if delivered.is_err() {
            self.hub.untrack(&request_id);
        }
        delivered
    }

    /// The `initialize` line for a freshly spawned child: the answer
    /// restates the model catalog, which the adapter keeps for `ListModels`
    /// once it confirms the reply matches this line's `request_id`.
    /// Best-effort after a spawn — a child whose stdin is already broken
    /// still opened, and `ListModels` refuses honestly until an answer
    /// lands.
    fn send_initialize(&self) {
        let request_id = self.next_control_id();
        let pending = PendingControl {
            kind: PendingControlKind::Initialize,
            request_id: request_id.clone(),
            wanted_model: None,
            previous_model: None,
            previous_fold_model: None,
            wanted_effort: None,
            previous_effort: None,
            sent_at: Instant::now(),
        };
        let _ = self.send_control(&initialize_control_request(&request_id), pending);
    }

    /// The `set_model` line for the live child, switching its model from
    /// the next turn with no relaunch. Best-effort: a delivery failure
    /// un-records the pick at the dispatch site, and a delivered line the
    /// child refuses rolls the pick back when the answer arrives.
    fn send_set_model(
        &self,
        model: &str,
        previous_model: Option<String>,
        previous_fold_model: Option<String>,
    ) -> Result<(), ProviderError> {
        let request_id = self.next_control_id();
        let pending = PendingControl {
            kind: PendingControlKind::SetModel,
            request_id: request_id.clone(),
            wanted_model: Some(model.to_owned()),
            previous_model,
            previous_fold_model,
            wanted_effort: None,
            previous_effort: None,
            sent_at: Instant::now(),
        };
        self.send_control(&set_model_control_request(&request_id, model), pending)
    }

    /// A switch onto a model the catalog says has no auto mode, made while
    /// the session is in Auto, asks the child into its asking mode and says
    /// why. The CLI accepts such a `set_model` and keeps reporting `auto`
    /// (probed live 2026-10-16: sonnet in auto → `set_model{haiku}` answered
    /// success, the next `initialize` still said `auto`, and
    /// `set_permission_mode{auto}` then failed with "auto mode unavailable
    /// for this model"), so without this the chip would claim a mode the
    /// model cannot run. The chip moves only on the mode the child reports
    /// back, as for any pick.
    fn leave_auto_if_unsupported(&self, model: &str) {
        let (in_auto, row) = {
            let fold = self.fold.lock().expect("fold mutex");
            (fold.permission_mode() == Some(PermissionMode::OnRequest), fold.catalog_row(model).cloned())
        };
        let Some(row) = row.filter(|row| in_auto && row.supports_auto_mode == Some(false)) else {
            return;
        };
        if self.send_set_permission_mode(PermissionMode::PromptUnmatched).is_ok() {
            self.hub.emit_card(
                CONTROL_MODE_FALLBACK_CARD,
                format!(
                    "Auto is not available for {}; Claude Code will ask before acting.",
                    row.display_name
                ),
            );
        }
    }

    /// The `apply_flag_settings` line raising the live child's effort from
    /// the next turn, with no relaunch. Best-effort: a delivery failure
    /// leaves the recorded level untouched at the dispatch site, and a
    /// delivered line the child refuses rolls the level back — and records
    /// a resume relaunch carrying it — when the answer arrives.
    fn send_apply_effort(&self, level: &str, previous_effort: Option<String>) -> Result<(), ProviderError> {
        let request_id = self.next_control_id();
        let pending = PendingControl {
            kind: PendingControlKind::ApplyEffort,
            request_id: request_id.clone(),
            wanted_model: None,
            previous_model: None,
            previous_fold_model: None,
            wanted_effort: Some(level.to_owned()),
            previous_effort,
            sent_at: Instant::now(),
        };
        self.send_control(&apply_effort_control_request(&request_id, level), pending)
    }

    /// The `set_permission_mode` line for the live child. The chip does not
    /// move here: the child's answer and `status` frame report the mode,
    /// and a refusal banners its reason.
    fn send_set_permission_mode(&self, mode: PermissionMode) -> Result<(), ProviderError> {
        let request_id = self.next_control_id();
        let pending = PendingControl {
            kind: PendingControlKind::SetPermissionMode(mode),
            request_id: request_id.clone(),
            wanted_model: None,
            previous_model: None,
            previous_fold_model: None,
            wanted_effort: None,
            previous_effort: None,
            sent_at: Instant::now(),
        };
        self.send_control(&set_permission_mode_control_request(&request_id, mode), pending)
    }

    /// The `apply_flag_settings` line putting the Read-only deny layer on
    /// the live child (`wanted` is Read-only) or taking it off (`wanted` is
    /// the mode being left for). A refusal banners and records a resume
    /// relaunch, which carries the layer exactly when the mode does.
    fn send_apply_read_only(&self, wanted: PermissionMode) -> Result<(), ProviderError> {
        let request_id = self.next_control_id();
        let pending = PendingControl {
            kind: PendingControlKind::ApplyReadOnly(wanted),
            request_id: request_id.clone(),
            wanted_model: None,
            previous_model: None,
            previous_fold_model: None,
            wanted_effort: None,
            previous_effort: None,
            sent_at: Instant::now(),
        };
        let on = wanted == PermissionMode::DenyUnmatched;
        self.send_control(&apply_read_only_control_request(&request_id, on), pending)
    }

    /// Record the mode a child was launched in: the chip shows it until the
    /// child's own report (the `initialize` answer, then each `init`)
    /// confirms or corrects it, and only a child launched in Full access
    /// may later switch into it live. A launch with no mode flag announces
    /// nothing — the child's report is the first word.
    fn note_launch_mode(&self, launch: &SessionLaunch) {
        let bypass = launch.permission_mode == Some(PermissionMode::AllowAll);
        *self.bypass_capable.lock().expect("bypass mutex") = bypass;
        // A fresh child holds only the layer its argv gave it: one applied
        // live died with the child it was applied to.
        *self.read_only_layer.lock().expect("read-only mutex") = if argv::carries_read_only_settings(launch) {
            ReadOnlyLayer::Launched
        } else {
            ReadOnlyLayer::Off
        };
        // A child launched in Full access already owes no relaunch into it.
        if bypass {
            *self.mode_relaunch_due.lock().expect("mode relaunch mutex") = None;
        }
        let moved = {
            let mut fold = self.fold.lock().expect("fold mutex");
            fold.set_session_id(&launch.session_id);
            launch.permission_mode.map(|mode| (mode, fold.note_permission_mode(mode)))
        };
        if let Some((mode, true)) = moved {
            let _ = self.tx.send(ProviderEvent::ApprovalModeChanged {
                session_id: launch.session_id.clone(),
                mode,
            });
        }
    }

    /// The relaunch a Full access pick owes, when one is due: `--resume`
    /// with the recorded model and effort plus `--permission-mode
    /// bypassPermissions`. The take is the claim. Pure — no spawn.
    fn take_mode_relaunch(&self, session_id: &str) -> Option<SessionLaunch> {
        let mode = self.mode_relaunch_due.lock().expect("mode relaunch mutex").take()?;
        let model = self.model.lock().expect("model mutex").clone();
        let effort = self.effort.lock().expect("effort mutex").clone();
        Some(argv::with_permission_mode(
            self.apply_use_own_mcp(argv::argv_for_resume(
                session_id,
                model.as_deref(),
                None,
                effort.as_deref(),
            )),
            Some(mode),
        ))
    }

    /// The `--resume` relaunch a turn needs when this adapter holds
    /// `session_id` but its child is gone — its stream ended on its own
    /// (the lane may have hung it up since), or it exited — carrying the
    /// recorded model and effort (the mode rides along in
    /// [`Self::relaunch`]). `None` when the child is alive, was never
    /// spawned, or the session is not this adapter's.
    fn revive_launch(&self, session_id: &str) -> Option<SessionLaunch> {
        self.check_session(session_id).ok()?;
        let lost = self.hub.child_lost.load(std::sync::atomic::Ordering::SeqCst);
        let exited = self.child.lock().expect("child mutex").as_mut().is_some_and(|running| running.has_exited());
        if !lost && !exited {
            return None;
        }
        let model = self.model.lock().expect("model mutex").clone();
        let effort = self.effort.lock().expect("effort mutex").clone();
        Some(self.apply_use_own_mcp(argv::argv_for_resume(
            session_id,
            model.as_deref(),
            None,
            effort.as_deref(),
        )))
    }

    /// Fold one raw child stdout line and confirm whatever it settles: the
    /// same [`ControlHub::ingest_line`] the live pump runs per line, so a
    /// scripted frame in tests settles exactly what the live child would.
    /// Public so scripted-frame tests drive confirmation without a child.
    pub fn ingest_child_line(&self, line: &str) {
        self.hub.ingest_line(line);
    }

    /// Control lines addressed so far, in order (see [`Self::send_control`]).
    #[cfg(test)]
    fn sent_control_lines(&self) -> Vec<String> {
        self.sent_control.lock().expect("sent control mutex").clone()
    }

    /// The config dir the history lookup reads for `session_id`: the
    /// owner's home while its legacy pin stands (the transcript still
    /// lives there — before consent nothing moved it), else the resolved
    /// Baaz home.
    fn config_dir_for(&self, session_id: &str) -> PathBuf {
        if crate::home::is_legacy_resume(session_id) {
            self.owner_config_dir()
        } else {
            self.resolved_config_dir()
        }
    }

    /// The home override for one spawn: `None` while `session_id`'s
    /// legacy pin stands (the child keeps the owner's real home — the
    /// scrub still applies), else the Baaz home's `CLAUDE_CONFIG_DIR`
    /// pair. Pure over the pin set plus the resolved home, so tests
    /// drive the routing without spawning.
    fn home_env_for(&self, session_id: &str) -> Option<Vec<(String, String)>> {
        if crate::home::is_legacy_resume(session_id) {
            None
        } else {
            Some(crate::home::child_env(&self.resolved_config_dir()))
        }
    }

    fn spawn_launch(&self, launch: &SessionLaunch) -> Result<Ack, ProviderError> {
        let mut child = self.child.lock().expect("child mutex");
        if child.is_some() {
            return Err(ProviderError::Rejected {
                reason: "this adapter already holds a session child; fork or resume instead"
                    .into(),
            });
        }
        // A pre-consent legacy resume spawns under the owner's real home
        // (no override, scrub kept) so `--resume` finds the transcript
        // where it still lives; everything else spawns Baaz-homed.
        let home_env = self.home_env_for(&launch.session_id);
        self.prepare_home(home_env.is_none());
        let running = match home_env {
            None => RunningChild::spawn_legacy(&self.program, launch, &self.hub),
            Some(_) => {
                let config = self.resolved_config_dir();
                RunningChild::spawn(&self.program, launch, &self.hub, &config)
            }
        }
        .map_err(|error| {
            ProviderError::Unavailable { reason: format!("could not spawn claude: {error}") }
        })?;
        *child = Some(running);
        // The guard drops here: `send_initialize` locks the child again to
        // write, and holding both would hang the open forever.
        drop(child);
        // Before `initialize` goes out: its answer reports the mode the
        // child really took, and must land after the launched one.
        self.note_launch_mode(launch);
        // The catalog arrives on the child's answer to this: without it
        // `ListModels` has nothing to serve but the offline fallback.
        self.send_initialize();
        *self.session_id.lock().expect("session mutex") = Some(launch.session_id.clone());
        // The launch's `--model`, when one was requested: the fold learns
        // the rest from the `init` frame, but an alias never appears there
        // under its own spelling, so the request is remembered as stated.
        if let Some(model) = &launch.model {
            *self.model.lock().expect("model mutex") = Some(model.clone());
        }
        // The launch's `--effort`, whatever it was — including none: the
        // next `SubmitInput` compares against this, so clearing back to
        // Default relaunches without the flag.
        *self.effort.lock().expect("effort mutex") = launch.effort.clone();
        // Remember the session's own workspace cwd for stored-history
        // lookup. Only a stated cwd counts: resume/fork launches carry
        // none, and an unknown cwd stays unknown (honest unavailable)
        // rather than guessed.
        if let Some(cwd) = &launch.cwd {
            *self.workspace.lock().expect("workspace mutex") = Some(PathBuf::from(cwd));
        }
        Ok(Ack::Session { session_id: launch.session_id.clone(), title: None })
    }

    fn require_child(&self) -> Result<(), ProviderError> {
        if self.child.lock().expect("child mutex").is_some() {
            Ok(())
        } else {
            Err(ProviderError::Unavailable { reason: "no session child is running".into() })
        }
    }

    fn check_session(&self, session_id: &str) -> Result<(), ProviderError> {
        match self.session_id.lock().expect("session mutex").as_deref() {
            Some(held) if held == session_id => Ok(()),
            Some(held) => Err(ProviderError::Rejected {
                reason: format!("this adapter holds session {held}, not {session_id}"),
            }),
            None => Err(ProviderError::Unavailable { reason: "no session child is running".into() }),
        }
    }

    /// Claim `session_id` without spawning: the offline stand-in for the
    /// `spawn_launch` claim, so tests can drive session-scoped commands
    /// (notably `SelectModel`) against a folded fixture transcript. Never
    /// spawns — the program name in tests does not exist on purpose.
    #[cfg(test)]
    fn attach_session_for_tests(&self, session_id: &str) {
        *self.session_id.lock().expect("session mutex") = Some(session_id.to_owned());
    }

    /// Answer one pending `can_use_tool` — or unknown-subtype — request
    /// with an explicit human decision. The pending lookup comes first: an
    /// unknown approval id is rejected even with no child running, and no
    /// path here invents an allow — without a live child the decision
    /// cannot be delivered, so the request goes back on its queue rather
    /// than being answered.
    fn decide_approval(
        &self,
        session_id: &str,
        approval: &str,
        choice: &str,
        feedback: Option<&str>,
    ) -> Result<Ack, ProviderError> {
        if !self.has_pending(approval) {
            return Err(ProviderError::Rejected {
                reason: format!("unknown approval {approval:?}: no pending request carries that id"),
            });
        }
        self.check_session(session_id)?;
        // Take-then-send: the removal is what claims the right to answer,
        // so two concurrent decisions for one id cannot both pass the
        // "is it pending" check — the loser finds nothing and fails
        // cleanly instead of writing a second `control_response`.
        let claimed = self.claim_approval(approval, choice, feedback)?;
        let delivered = match self.child.lock().expect("child mutex").as_mut() {
            Some(running) => running.send_line(claimed.line()).map_err(|error| {
                ProviderError::Unavailable {
                    reason: format!("the session child is unreachable: {error}"),
                }
            }),
            None => Err(ProviderError::Unavailable {
                reason: "no session child is running".into(),
            }),
        };
        match delivered {
            Ok(()) => Ok(Ack::Accepted),
            Err(error) => {
                // Undeliverable, so unanswered: put the claim back rather
                // than dropping a decision the child never saw.
                let mut fold = self.fold.lock().expect("fold mutex");
                match claimed {
                    ClaimedApproval::Known(request, _) => fold.requeue_approval(request),
                    ClaimedApproval::Unknown(request, _) => fold.requeue_unknown(request),
                }
                Err(error)
            }
        }
    }

    /// Whether any queue holds `approval`: known or unknown-subtype.
    fn has_pending(&self, approval: &str) -> bool {
        let fold = self.fold.lock().expect("fold mutex");
        fold.pending_approvals().iter().any(|queued| queued.request_id == approval)
            || fold.pending_unknown().iter().any(|queued| queued.request_id == approval)
    }

    /// Claim the single right to answer `approval` and mint its
    /// `control_response` line, in one lock hold: the removal is the
    /// claim. A second caller finds nothing pending and gets a clean
    /// rejection. Unknown ids and unknown choices are rejected without
    /// consuming any claim, so a typo never eats the request.
    fn claim_approval(
        &self,
        approval: &str,
        choice: &str,
        feedback: Option<&str>,
    ) -> Result<ClaimedApproval, ProviderError> {
        let mut fold = self.fold.lock().expect("fold mutex");
        let known = fold.pending_approvals().iter().any(|queued| queued.request_id == approval);
        let unknown = fold.pending_unknown().iter().any(|queued| queued.request_id == approval);
        if !known && !unknown {
            return Err(ProviderError::Rejected {
                reason: format!("unknown approval {approval:?}: no pending request carries that id"),
            });
        }
        if choice != "allow" && choice != "deny" {
            return Err(ProviderError::Rejected {
                reason: format!(
                    "unknown approval choice {choice:?} for {approval}: offer \"allow\" or \"deny\""
                ),
            });
        }
        if known {
            let request = fold.take_approval(approval).expect("presence checked above");
            if choice == "deny" {
                fold.note_denied(approval);
            }
            let line = fold::decide_approval(&request, choice, feedback)?;
            Ok(ClaimedApproval::Known(request, line))
        } else {
            let request = fold.take_unknown(approval).expect("presence checked above");
            let line = fold::decide_unknown_approval(&request, choice, feedback)?;
            Ok(ClaimedApproval::Unknown(request, line))
        }
    }

    /// The relaunch a `SubmitInput` needs when its effort clears back to
    /// Default while a level is in effect: `--resume <session-id>` without
    /// `--effort`, carrying the recorded `--model` along. Raising a level
    /// rides `apply_flag_settings` instead (see the `SubmitInput` arm), so
    /// only the clearing direction builds a launch here. `None` means the
    /// running child already flies this level and the turn goes straight
    /// to stdin. Pure — no spawn — so tests drive this without a CLI.
    ///
    /// The fold (and its transcript) outlives the swap: it lives on the
    /// adapter, not the child. And a resumed child does not replay history,
    /// so nothing already folded is emitted twice.
    fn resume_launch_for_effort(
        &self,
        session_id: &str,
        effort: Option<&str>,
    ) -> Option<SessionLaunch> {
        let wanted = effort.filter(|effort| !effort.is_empty()).map(str::to_owned);
        if *self.effort.lock().expect("effort mutex") == wanted {
            return None;
        }
        let model = self.model.lock().expect("model mutex").clone();
        Some(self.apply_use_own_mcp(argv::argv_for_resume(
            session_id,
            model.as_deref(),
            None,
            wanted.as_deref(),
        )))
    }

    /// Every relaunch is the same session: one built without a mode flag
    /// (an effort swap, a revive, a refused-layer fallback) keeps the mode
    /// the child was in rather than dropping back to the CLI's default —
    /// and with Read-only, its deny layer on `--settings`. Pure — no spawn.
    fn with_session_mode(&self, launch: SessionLaunch) -> SessionLaunch {
        match launch.permission_mode {
            Some(_) => launch,
            None => {
                let mode = self.fold.lock().expect("fold mutex").permission_mode();
                argv::with_permission_mode(launch, mode)
            }
        }
    }

    /// Swap the running child for `launch`: hang up the old one first, then
    /// spawn. The old child's pump ends with its stdout; its folded
    /// transcript stays on the adapter. The relay rides along: an
    /// effort-swap relaunch is still the same session, so the resumed child
    /// sees the same bridge the old one did.
    fn relaunch(&self, launch: &SessionLaunch) -> Result<(), ProviderError> {
        if let Some(running) = self.child.lock().expect("child mutex").as_mut() {
            running.shutdown();
        }
        *self.child.lock().expect("child mutex") = None;
        // Requests the old child never answered die with it (Y1b review).
        self.hub.forget_pending();
        let launch = self.with_session_mode(self.with_mcp_config(launch.clone())?);
        // An effort-swap relaunch is still the same session: a legacy
        // resume keeps the owner's home here too.
        let home_env = self.home_env_for(&launch.session_id);
        self.prepare_home(home_env.is_none());
        let running = match home_env {
            None => RunningChild::spawn_legacy(&self.program, &launch, &self.hub),
            Some(_) => {
                let config = self.resolved_config_dir();
                RunningChild::spawn(&self.program, &launch, &self.hub, &config)
            }
        }
        .map_err(|error| {
            ProviderError::Unavailable { reason: format!("could not respawn claude: {error}") }
        })?;
        *self.child.lock().expect("child mutex") = Some(running);
        // A relaunched child restates the whole session, catalog included
        // — and its mode, which must land after the launched one.
        self.note_launch_mode(&launch);
        self.send_initialize();
        *self.session_id.lock().expect("session mutex") = Some(launch.session_id.clone());
        if let Some(model) = &launch.model {
            *self.model.lock().expect("model mutex") = Some(model.clone());
        }
        *self.effort.lock().expect("effort mutex") = launch.effort.clone();
        if let Some(cwd) = &launch.cwd {
            *self.workspace.lock().expect("workspace mutex") = Some(PathBuf::from(cwd));
        }
        Ok(())
    }

    /// Split neutral submission parts into the turn's text plus its image
    /// parts: text joins in order (the lane's long-standing rule), images
    /// ride as base64 `source` blocks (probed live 2026-09-26,
    /// `fixtures/claude-code/image.jsonl`).
    fn split_parts(parts: &[provider::SubmissionPart]) -> (String, Vec<argv::ImageInput>) {
        let mut text = String::new();
        let mut images = Vec::new();
        for part in parts {
            match part {
                provider::SubmissionPart::Text(chunk) => text.push_str(chunk),
                provider::SubmissionPart::Image { base64_data, media_type } => {
                    images.push(argv::ImageInput {
                        base64_data: base64_data.clone(),
                        media_type: media_type.clone(),
                    });
                }
            }
        }
        (text, images)
    }

    fn submit_parts(
        &self,
        session_id: &str,
        text: &str,
        images: &[argv::ImageInput],
        turn_id: String,
        display_text: Option<&str>,
    ) -> Result<Ack, ProviderError> {
        self.check_session(session_id)?;
        // The full text goes to stdin; the bubble shows the display text
        // when one rode the submit, keyed by the full text the replayed
        // user message echo carries back.
        if let Some(display) = display_text {
            self.fold.lock().expect("fold mutex").record_display_text(text, display);
        }
        let line = argv::user_content_line(text, images);
        // Noted before the line goes out: the pump may fold the turn's
        // first message before this thread locks the fold again.
        self.fold.lock().expect("fold mutex").note_submission(&turn_id);
        let sent = match self.child.lock().expect("child mutex").as_mut() {
            Some(running) => running.send_line(&line).map_err(|error| ProviderError::Unavailable {
                reason: format!("the session child is unreachable: {error}"),
            }),
            None => Err(ProviderError::Unavailable { reason: "no session child is running".into() }),
        };
        if let Err(error) = sent {
            self.fold.lock().expect("fold mutex").forget_submission(&turn_id);
            return Err(error);
        }
        // The ack carries the submission handle: the CLI names the turn
        // only when its first message streams, and the fold announces
        // that id as `TurnIdentified` against this one.
        Ok(Ack::TurnAccepted { turn_id })
    }

    fn find_stored(&self, session_id: &str) -> Option<PathBuf> {
        // The doc §3 rule: resolve the session's own workspace cwd, slug
        // it, look in that one directory. An unresolvable cwd, an absent
        // slug directory, or a missing file all degrade to None — the
        // callers answer honest unavailable. Never scan other directories
        // when the workspace is known: that would serve one workspace's
        // transcript inside another.
        let config = self.config_dir_for(session_id);
        let workspace = self.workspace.lock().expect("workspace mutex").clone();
        match workspace {
            Some(workspace) => history::transcript_path_for_config(&workspace, session_id, &config)
                .filter(|candidate| candidate.is_file()),
            // A fresh adapter after a restart holds no workspace (resume
            // carries no cwd): fall back to an exact-id scan over every
            // slug directory. The session id is a UUID and the match is
            // the exact `<id>.jsonl` filename — zero or several matches
            // still refuse — so this locates the session's own file
            // without ever serving a neighbor's.
            None => Self::scan_stored(&config, session_id),
        }
    }

    /// The stored transcript for `session_id` by exact filename, over
    /// every slug directory under `<config>/projects`. `Some` only on
    /// exactly one match: none — or two files claiming one id — refuses.
    fn scan_stored(config: &std::path::Path, session_id: &str) -> Option<PathBuf> {
        if session_id.is_empty() || session_id.contains('/') {
            return None;
        }
        let projects = config.join("projects");
        let entries = std::fs::read_dir(&projects).ok()?;
        let file_name = format!("{session_id}.jsonl");
        let mut hits = Vec::new();
        for entry in entries.filter_map(|entry| entry.ok()) {
            let candidate = entry.path().join(&file_name);
            if candidate.is_file() {
                hits.push(candidate);
            }
        }
        match hits.len() {
            1 => hits.pop(),
            _ => None,
        }
    }

    /// Best-effort resume seeding: mark every user echo uuid the stored
    /// transcript already carries, so the resumed child's stream cannot
    /// re-bubble history the adapter already showed. Returns the marked
    /// count. Zero when no file is found, the file is unreadable, or the
    /// scan is ambiguous — the same honest-unavailable cases as
    /// [`Self::find_stored`] — in which case no disk history exists to
    /// collide with, and the fold still dedupes within the stream itself.
    /// A fresh adapter resuming without a workspace locates the file by
    /// exact `<id>.jsonl` match (see [`Self::scan_stored`]), never by
    /// guessing a neighbor's.
    fn seed_history_echoes(&self, session_id: &str) -> usize {
        let Some(path) = self.find_stored(session_id) else { return 0 };
        let Ok(text) = std::fs::read_to_string(&path) else { return 0 };
        let mut fold = self.fold.lock().expect("fold mutex");
        let mut marked = 0;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(frame) = frame::decode_line(line) else { continue };
            if let frame::Frame::UserText { uuid, .. } = frame {
                if !uuid.is_empty() {
                    fold.mark_user_echo_seen(&uuid);
                    marked += 1;
                }
            }
        }
        marked
    }

    fn stored_deltas(&self, session_id: &str) -> Result<Vec<Delta>, ProviderError> {
        self.stored_history(session_id).map(|(deltas, _)| deltas)
    }

    /// The stored transcript folded to deltas plus the session's model:
    /// what a reopen shows before any live delta. Still-open turns close
    /// with their stored footers (the file carries no `result` frame), so
    /// a reopened view settles instead of hanging on "working".
    fn stored_history(
        &self,
        session_id: &str,
    ) -> Result<(Vec<Delta>, Option<String>), ProviderError> {
        let Some(path) = self.find_stored(session_id) else {
            return Err(ProviderError::Unavailable {
                reason: format!(
                    "no stored transcript for session {session_id}: a resumed child does not \
                     replay history, and no file names it — refusing to guess a different file"
                ),
            });
        };
        let text = std::fs::read_to_string(&path).map_err(|error| ProviderError::Unavailable {
            reason: format!("stored transcript is unreadable: {error}"),
        })?;
        let mut fold = ClaudeFold::new();
        // The live fold holds this session's submit display map; the
        // replay fold is fresh, so it inherits the map before the stored
        // lines fold — a reopened handoff pack still bubbles its summary.
        let seeded = self.fold.lock().expect("fold mutex").display_overrides();
        fold.set_display_overrides(seeded);
        let mut deltas = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(frame) = frame::decode_line(line) else { continue };
            deltas.extend(fold.apply(&frame));
        }
        deltas.extend(fold.finish_stored_turns());
        Ok((deltas, fold.stored_model().map(str::to_owned)))
    }
}

/// The human line behind a failed `--version`: the stream's first
/// non-empty line, trimmed to 200 characters — never the whole stream, and
/// never compared against the version floor.
fn version_failure_line(stream: &[u8], status: &std::process::ExitStatus) -> String {
    let line = String::from_utf8_lossy(stream)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .chars()
        .take(200)
        .collect::<String>();
    if line.is_empty() {
        format!("exited with {status}")
    } else {
        line
    }
}

impl ProviderAdapter for ClaudeCodeAdapter {
    fn id(&self) -> ProviderId {
        aui_protocol::Provider::Claude
    }

    fn connect(&mut self, _client: &ConnectInfo) -> Result<Handshake, ProviderError> {
        if *self.connected.lock().expect("connected mutex") {
            return Err(ProviderError::Rejected { reason: "already connected".into() });
        }
        // The version floor is enforced here, before any session exists:
        // `claude --version` prints `2.1.276 (Claude Code)` and the shared
        // parser tolerates that trailer. The child's `PATH` is the
        // login-shell `PATH` with the program's own directory first, so a
        // Dock launch still runs a home install and its `env`-shebang
        // neighbours; the inherited desktop-agent env is scrubbed (see
        // [`provider::child_env`]). No Baaz home is set: a version check
        // reads no store.
        let mut version_command = std::process::Command::new(&self.program);
        version_command
            .arg("--version")
            .env("PATH", provider::env_path::child_path_for(std::path::Path::new(&self.program)));
        provider::child_env::scrub_command(&mut version_command);
        let output = version_command
            .output()
            .map_err(|error| ProviderError::Unavailable {
                reason: format!("could not ask claude its version: {error}"),
            })?;
        if !output.status.success() {
            return Err(ProviderError::Unavailable {
                reason: format!(
                    "claude --version failed: {}",
                    version_failure_line(&output.stderr, &output.status)
                ),
            });
        }
        let raw = String::from_utf8_lossy(&output.stdout);
        let version = raw.trim().to_owned();
        // Only stdout of a successful exit is ever compared against the
        // floor: stderr of a failed spawn is a spawn failure, never a
        // version.
        if provider::parse_version(&version).is_none() {
            return Err(ProviderError::Unavailable {
                reason: format!(
                    "could not read claude version from: {}",
                    version_failure_line(&output.stdout, &output.status)
                ),
            });
        }
        if !claude_version_supported(&version) {
            return Err(ProviderError::Rejected {
                reason: format!(
                    "claude {version} is below the floor {CLAUDE_VERSION_FLOOR}; refusing"
                ),
            });
        }
        *self.connected.lock().expect("connected mutex") = true;
        *self.version.lock().expect("version mutex") = Some(version.clone());
        Ok(Handshake {
            provider: aui_protocol::Provider::Claude,
            agent_name: "claude-code".into(),
            agent_version: version,
        })
    }

    fn capabilities(&self) -> CapabilitySet {
        capabilities()
    }

    fn dispatch(&self, command: Command) -> Result<Ack, ProviderError> {
        // Settle whatever the pump folded since the last command: a refusal
        // the child already answered must roll back before this command
        // reads the record — never one command later.
        self.hub.drain_confirmations();
        match command {
            Command::OpenSession { request_id, workspace, model, mode, .. } => {
                let launch =
                    self.launch_for_open(&request_id, workspace.as_deref(), model.as_deref())?;
                self.spawn_launch(&argv::with_permission_mode(launch, mode))
            }
            Command::ResumeSession { session_id, mode, .. } => {
                let launch = argv::with_permission_mode(self.launch_for_resume(&session_id)?, mode);
                let ack = self.spawn_launch(&launch)?;
                // Best-effort resume seeding (see `seed_history_echoes`):
                // history already shown must not bubble again off the
                // resumed child's stream.
                self.seed_history_echoes(&session_id);
                // The resumed child replays no transcript on stdout, so
                // the lane shows the stored file instead: folded here and
                // emitted before any live delta, exactly once per resume
                // (the emission below is the only history source — the
                // child never repeats it). No file, no history: the lane
                // opens empty and the next turn still appends.
                if let Ok((deltas, model)) = self.stored_history(&session_id) {
                    if let Some(model) = model {
                        *self.model.lock().expect("model mutex") = Some(model.clone());
                        self.fold.lock().expect("fold mutex").set_model(&model);
                    }
                    if !deltas.is_empty() {
                        let _ = self.tx.send(provider::ProviderEvent::Deltas {
                            session_id: Some(session_id.clone()),
                            deltas,
                        });
                    }
                }
                Ok(ack)
            }
            Command::ForkSession { request_id, session_id, mode, .. } => {
                let launch = self.launch_for_fork(&request_id, &session_id)?;
                self.spawn_launch(&argv::with_permission_mode(launch, mode))
            }
            Command::ListSessions { workspace, .. } => {
                let projects = self.resolved_config_dir().join("projects");
                let mut ids = Vec::new();
                match workspace {
                    Some(workspace) => {
                        let resolved = history::resolve_cwd(std::path::Path::new(&workspace))
                            .map_err(|_| ProviderError::Unavailable {
                                reason: format!(
                                    "workspace {workspace} does not resolve; refusing to guess a slug"
                                ),
                            })?;
                        let dir = projects.join(history::slug_for_cwd(&resolved));
                        ids.extend(history::stored_session_ids(&dir));
                    }
                    None => {
                        let Ok(entries) = std::fs::read_dir(&projects) else {
                            return Ok(Ack::SessionIndex { sessions: Vec::new(), next_cursor: None });
                        };
                        for entry in entries.filter_map(|entry| entry.ok()) {
                            ids.extend(history::stored_session_ids(&entry.path()));
                        }
                    }
                }
                Ok(Ack::SessionIndex {
                    sessions: ids
                        .into_iter()
                        .map(|session_id| SessionSummary { session_id, title: None })
                        .collect(),
                    next_cursor: None,
                })
            }
            Command::ReadSession { session_id, .. } => match self.find_stored(&session_id) {
                Some(_) => Ok(Ack::Session { session_id, title: None }),
                None => Err(ProviderError::Unavailable {
                    reason: format!(
                        "no stored transcript for session {session_id}: refusing to guess a \
                         different file"
                    ),
                }),
            },
            Command::CompactSession { .. } => Err(ProviderError::unsupported(
                "compact-session",
                "no compact-now command exists over --print; compaction happens when the \
                 --autocompact window fills, not when asked",
            )),
            Command::SelectModel { session_id, model, .. } => {
                self.check_session(&session_id)?;
                // Mid-session switch, no relaunch: `set_model` reaches the
                // live child on the control channel and the next turn runs
                // on the new model (probed live 2026-09-28: haiku → sonnet
                // answered success and the assistant message reported
                // `claude-sonnet-5`). The recorded pick is the session's
                // effective model from here on — the chip, the next
                // footers, and the next resume's `--model`. Applies to a
                // session with turns in it exactly as to a fresh one:
                // nothing here counts turns. The pick is optimistic: the
                // child's answer confirms it, and a refusal — or a silence
                // past the timeout — rolls it back and banners the reason
                // (see [`ControlHub`]). A delivery failure un-records the
                // pick at once, so the chip never claims a model the child
                // never took.
                let previous_model = self.model.lock().expect("model mutex").clone();
                let previous_fold =
                    self.fold.lock().expect("fold mutex").model().map(str::to_owned);
                *self.model.lock().expect("model mutex") = Some(model.clone());
                self.fold.lock().expect("fold mutex").set_model(&model);
                if let Err(error) =
                    self.send_set_model(&model, previous_model.clone(), previous_fold.clone())
                {
                    *self.model.lock().expect("model mutex") = previous_model;
                    let mut fold = self.fold.lock().expect("fold mutex");
                    match previous_fold {
                        Some(previous) => fold.set_model(&previous),
                        None => fold.clear_model(),
                    }
                    return Err(error);
                }
                self.leave_auto_if_unsupported(&model);
                Ok(Ack::Accepted)
            }
            Command::SelectApprovalMode { session_id, mode, .. } => {
                self.check_session(&session_id)?;
                // Full access is the one mode a child cannot switch into
                // live unless it was launched in it: the pick is admitted
                // now, shown now, and lands as a `--resume` relaunch before
                // the next turn — the next turn is the first one it could
                // govern anyway.
                if mode == PermissionMode::AllowAll
                    && !*self.bypass_capable.lock().expect("bypass mutex")
                {
                    *self.mode_relaunch_due.lock().expect("mode relaunch mutex") = Some(mode);
                    if self.fold.lock().expect("fold mutex").note_permission_mode(mode) {
                        let _ = self.tx.send(ProviderEvent::ApprovalModeChanged { session_id, mode });
                    }
                    return Ok(Ack::Accepted);
                }
                // Read-only is `dontAsk` plus its deny layer (see
                // `argv::read_only_settings`): the layer goes on live with
                // the switch in, and comes off live only when it went on
                // live — a layer that rode `--settings` sheds by relaunch,
                // admitted and shown now like Full access above.
                let into_read_only = mode == PermissionMode::DenyUnmatched;
                let layer = *self.read_only_layer.lock().expect("read-only mutex");
                match (into_read_only, layer) {
                    (true, ReadOnlyLayer::Off) => {
                        self.send_apply_read_only(mode)?;
                        *self.read_only_layer.lock().expect("read-only mutex") = ReadOnlyLayer::Live;
                    }
                    (false, ReadOnlyLayer::Launched) => {
                        *self.mode_relaunch_due.lock().expect("mode relaunch mutex") = Some(mode);
                        if self.fold.lock().expect("fold mutex").note_permission_mode(mode) {
                            let _ = self.tx.send(ProviderEvent::ApprovalModeChanged { session_id, mode });
                        }
                        return Ok(Ack::Accepted);
                    }
                    (false, ReadOnlyLayer::Live) => {
                        self.send_apply_read_only(mode)?;
                        *self.read_only_layer.lock().expect("read-only mutex") = ReadOnlyLayer::Off;
                    }
                    _ => {}
                }
                // Every other pick rides the control channel, and replaces
                // a pick still waiting on its relaunch.
                *self.mode_relaunch_due.lock().expect("mode relaunch mutex") = None;
                self.send_set_permission_mode(mode)?;
                Ok(Ack::Accepted)
            }
            Command::RunShell { .. } => Err(ProviderError::unsupported(
                "run-shell",
                "no out-of-turn shell surface was probed over stream-json stdin; the Bash tool \
                 runs inside turns",
            )),
            Command::SubmitInput { request_id, session_id, parts, effort, display_text, .. } => {
                // Effort rides the control channel: when the pick differs
                // from the level in effect, `apply_flag_settings` raises it
                // from the next turn with no relaunch (probed live
                // 2026-09-28 — the next turn's session jsonl carried
                // `"effort":"high"`). Same level — or none anywhere — skips
                // the send and the turn goes straight in. Clearing back to
                // Default has no verified control-channel reset, so only
                // that direction keeps the `--resume` relaunch path (the
                // relaunched child spawns without `--effort`). A rejected
                // level never sticks silently: the answer rolls the recorded
                // level back to where the child actually is, banners the
                // CLI's reason, and records a `--resume` relaunch carrying
                // the level — performed above, before the next turn — so
                // the effort lands after all.
                // A refused `apply_flag_settings` records a resume relaunch
                // carrying the level (the pump owns no spawn): perform it
                // here, before the turn goes out, so the turn runs at the
                // picked level after all. A relaunch that cannot spawn
                // fails this submit loudly — never silently.
                if let Some(launch) = self.hub.take_effort_relaunch() {
                    self.check_session(&session_id)?;
                    self.relaunch(&launch)?;
                }
                // The child is gone (it exited, or the lane hung up on a
                // lost connection): the session lives on in its transcript,
                // so this turn resumes it rather than failing against a
                // dead stdin.
                if let Some(launch) = self.revive_launch(&session_id) {
                    self.relaunch(&launch)?;
                }
                // A mode pick the old child could not take live (Full
                // access, or out of a launched Read-only): relaunch into it
                // before the turn goes out.
                if let Some(launch) = self.take_mode_relaunch(&session_id) {
                    self.check_session(&session_id)?;
                    self.relaunch(&launch)?;
                }
                let wanted = effort.filter(|effort| !effort.is_empty());
                let recorded = self.effort.lock().expect("effort mutex").clone();
                if wanted != recorded {
                    match wanted.clone() {
                        Some(level) => {
                            self.check_session(&session_id)?;
                            self.send_apply_effort(&level, recorded)?;
                            *self.effort.lock().expect("effort mutex") = Some(level);
                        }
                        None => {
                            if let Some(launch) =
                                self.resume_launch_for_effort(&session_id, None)
                            {
                                self.check_session(&session_id)?;
                                self.relaunch(&launch)?;
                            }
                        }
                    }
                }
                let (text, images) = Self::split_parts(&parts);
                self.submit_parts(&session_id, &text, &images, request_id, display_text.as_deref())
            }
            // Unverified, so attempted: a second stdin frame mid-turn is the
            // only lane the process shape offers, unprobed as it is.
            Command::SteerInput { request_id, session_id, parts, .. } => {
                let _ = request_id;
                let (text, images) = Self::split_parts(&parts);
                self.check_session(&session_id)?;
                let line = argv::user_content_line(&text, &images);
                match self.child.lock().expect("child mutex").as_mut() {
                    Some(running) => {
                        running.send_line(&line).map_err(|error| ProviderError::Unavailable {
                            reason: format!("the session child is unreachable: {error}"),
                        })?;
                        Ok(Ack::Accepted)
                    }
                    None => Err(ProviderError::Unavailable {
                        reason: "no session child is running".into(),
                    }),
                }
            }
            // TurnControl is Unverified: interrupt/cancel over stdin was
            // never probed, and spelling it as success would be the lie this
            // seam exists to prevent.
            Command::InterruptTurn { .. } => Err(ProviderError::Rejected {
                reason: "interrupt over stream-json stdin was never probed; not attempted blind"
                    .into(),
            }),
            Command::CancelTurn { .. } => Err(ProviderError::Rejected {
                reason: "cancel over stream-json stdin was never probed; not attempted blind".into(),
            }),
            Command::ReclaimQueued { .. } => Err(ProviderError::Rejected {
                reason: "no queued-turn lane was probed over stream-json stdin".into(),
            }),
            Command::ListModels { session } => {
                // The catalog the child's `initialize` answer restated (see
                // `send_initialize`): `value` is the id, `displayName` the
                // label, the description plus the resolved full id the
                // detail line, each row's own `supportedEffortLevels` the
                // effort list. Empty until the first answer lands — then
                // this refuses honestly and the lane falls back to Baaz's
                // supplied alias list, rather than serving that fallback as
                // the child's own.
                let rows =
                    self.fold.lock().expect("fold mutex").catalog_models().to_vec();
                if rows.is_empty() {
                    return Err(ProviderError::Unavailable {
                        reason: "no initialize answer has landed yet; the catalog is unknown"
                            .into(),
                    });
                }
                let active = session
                    .as_deref()
                    .and_then(|session| {
                        (Some(session) == self.session_id.lock().expect("session mutex").as_deref())
                            .then(|| self.model.lock().expect("model mutex").clone())
                            .flatten()
                    })
                    .unwrap_or_default();
                Ok(Ack::ModelCatalog {
                    models: rows
                        .into_iter()
                        .map(|row| ModelSummary {
                            active: row.value == active,
                            label: row.display_name.clone(),
                            id: row.value.clone(),
                            efforts: row.efforts.clone(),
                            unavailable_modes: row.unavailable_modes(),
                            hidden: false,
                            is_default: row.value == "default",
                            description: match (&row.description, &row.resolved_model) {
                                (Some(description), Some(resolved))
                                    if resolved != &row.value =>
                                {
                                    Some(format!("{description} ({resolved})"))
                                }
                                (Some(description), _) => Some(description.clone()),
                                (None, Some(resolved)) if resolved != &row.value => {
                                    Some(resolved.clone())
                                }
                                (None, _) => None,
                            },
                        })
                        .collect(),
                    provider: "anthropic".into(),
                })
            }
            // The control channel answers here: a pending `can_use_tool`
            // — or unknown-subtype — request is decided with one of its
            // card's own choices (`"allow"`/`"deny"`), written to the
            // child as a `control_response`. Unseen ids and unknown
            // choices are rejected; nothing is ever auto-allowed.
            Command::DecideApproval { session_id, approval, choice, feedback, .. } => {
                self.decide_approval(&session_id, &approval, &choice, feedback.as_deref())
            }
            Command::ListPending { session_id } => {
                self.require_child()?;
                self.check_session(&session_id)?;
                let fold = self.fold.lock().expect("fold mutex");
                Ok(Ack::PendingWork {
                    approvals: fold
                        .pending_approvals()
                        .iter()
                        .map(|request| PendingApproval {
                            id: request.request_id.clone(),
                            session_id: session_id.clone(),
                            headline: frame::approval_headline(request),
                            stage_token: None,
                        })
                        .collect(),
                    questions: Vec::new(),
                })
            }
            // Unreachable through the gate (Questions is Unavailable);
            // refused here too, so the raw dispatch can never spell it Ok.
            Command::AnswerQuestion { .. } => Err(ProviderError::unsupported(
                "answer-question",
                "Claude Code asks in prose; there is no question id to answer",
            )),
            Command::DismissQuestion { .. } => Err(ProviderError::unsupported(
                "dismiss-question",
                "Claude Code asks in prose; there is no question id to answer",
            )),
            Command::ClarifyQuestion { .. } => Err(ProviderError::unsupported(
                "clarify-question",
                "Claude Code asks in prose; there is no question id to answer",
            )),
            Command::PageTranscript { session_id, after, limit, backward } => {
                let deltas = self.stored_deltas(&session_id)?;
                let len = deltas.len();
                let limit = (limit as usize).max(1);
                let (start, end) = if backward {
                    let end = after
                        .as_deref()
                        .and_then(|cursor| cursor.parse::<usize>().ok())
                        .unwrap_or(len)
                        .min(len);
                    (end.saturating_sub(limit), end)
                } else {
                    let start = after
                        .as_deref()
                        .and_then(|cursor| cursor.parse::<usize>().ok())
                        .unwrap_or(0)
                        .min(len);
                    (start, (start + limit).min(len))
                };
                let next_cursor = if backward {
                    if start > 0 { Some(start.to_string()) } else { None }
                } else if end < len {
                    Some(end.to_string())
                } else {
                    None
                };
                Ok(Ack::TranscriptPage { deltas: deltas[start..end].to_vec(), next_cursor })
            }
            Command::FollowSession { .. } => {
                self.require_child()?;
                Ok(Ack::Accepted)
            }
            Command::UnfollowSession { .. } => Ok(Ack::Accepted),
            Command::ReadStoredOutput { .. } => Err(ProviderError::unsupported(
                "read-stored-output",
                "the CLI exposes no stored-output reference; page the stored transcript instead",
            )),
            Command::ReadAccount => {
                let fold = self.fold.lock().expect("fold mutex");
                let label = fold.account().label();
                // `claude --version` succeeds whether or not the CLI is
                // authenticated, so `connect()` proves nothing about auth,
                // and this crate has no `auth status` probe — nothing here
                // runs the CLI beyond spawn/version. The only auth evidence
                // this adapter ever observes is a rate-limit meter reading
                // from the live child: the CLI only emits one while making
                // authenticated API calls. So `signed_in` is true exactly
                // when a reading has been seen (label present); `false`
                // means "no login observed", never "definitely logged out".
                // A fresh or logged-out CLI reads false — fail closed, never
                // claim a login nobody checked.
                Ok(Ack::Account { signed_in: label.is_some(), label })
            }
            Command::BeginLogin { .. } => Err(ProviderError::unsupported(
                "begin-login",
                "Claude Code authenticates outside the session; use `claude auth`",
            )),
            Command::CancelLogin => Err(ProviderError::unsupported(
                "cancel-login",
                "Claude Code authenticates outside the session; use `claude auth`",
            )),
            Command::LogOut => Err(ProviderError::unsupported(
                "log-out",
                "Claude Code authenticates outside the session; use `claude auth`",
            )),
        }
    }

    fn events(&self) -> Receiver<ProviderEvent> {
        self.rx.clone()
    }

    /// The fold's latest `rate_limit_event`, if any. A peek only — no
    /// wire call — and `None` while the pump holds the fold, so a menu
    /// refresh never blocks on a streaming turn.
    fn read_usage(&self) -> Option<provider::UsageReport> {
        self.fold.try_lock().ok()?.account().usage_report()
    }

    fn shutdown(&mut self) {
        if let Some(running) = self.child.lock().expect("child mutex").as_mut() {
            running.shutdown();
        }
        *self.child.lock().expect("child mutex") = None;
    }
}

impl Drop for ClaudeCodeAdapter {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meter_line() -> &'static str {
        r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","resetsAt":1790384400,"rateLimitType":"seven_day","utilization":0.97,"isUsingOverage":true,"surpassedThreshold":0.75,"unifiedWindows":{"five_hour":{"utilization":0.9,"resetsAt":1790187000},"seven_day":{"utilization":0.97,"resetsAt":1790384400}}}}"#
    }

    #[test]
    fn read_account_is_false_until_a_meter_reading_is_seen() {
        // Fresh adapter: no meter observed, so no login claimed. A
        // hardcoded `true` fails this test.
        let adapter = ClaudeCodeAdapter::new("claude");
        let ack = adapter.dispatch(Command::ReadAccount).expect("account reads");
        assert!(
            matches!(ack, Ack::Account { signed_in: false, label: None }),
            "no reading seen, no login claimed: {ack:?}"
        );
    }

    /// Seed an executable fake binary in a fresh temp dir.
    fn seed_executable(tag: &str, name: &str, body: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "baaz-claude-shebang-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(name);
        std::fs::write(&path, body).expect("fake writes");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut perms = std::fs::metadata(&path).expect("fake metadata").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).expect("fake executable");
        }
        (dir, path)
    }

    #[test]
    fn a_failed_version_is_never_below_the_floor() {
        // The Dock failure shape: a non-zero exit with the spawn error on
        // stderr and empty stdout. The old code compared that stderr
        // against the floor and refused with "… is below the floor …".
        let (dir, claude) = seed_executable(
            "fail",
            "claude",
            "#!/bin/sh\necho 'env: node: No such file or directory' >&2\nexit 127\n",
        );
        let mut adapter = ClaudeCodeAdapter::new(&claude.to_string_lossy());
        let error = adapter.connect(&ConnectInfo::new("baaz", "0.0.0")).expect_err("127 fails");
        match error {
            ProviderError::Unavailable { reason } => {
                assert!(reason.contains("--version failed"), "honest failure: {reason}");
                assert!(reason.contains("env: node: No such file or directory"), "stderr kept: {reason}");
                assert!(!reason.contains("below the floor"), "never a floor refusal: {reason}");
            }
            other => panic!("a spawn failure is unavailable, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unparseable_version_is_never_below_the_floor() {
        // Exit 0 but no version on stdout: nothing to compare against the
        // floor, so the probe says what it could not read.
        let (dir, claude) =
            seed_executable("garbage", "claude", "#!/bin/sh\necho 'not a version'\nexit 0\n");
        let mut adapter = ClaudeCodeAdapter::new(&claude.to_string_lossy());
        let error = adapter.connect(&ConnectInfo::new("baaz", "0.0.0")).expect_err("garbage fails");
        match error {
            ProviderError::Unavailable { reason } => {
                assert!(reason.contains("could not read claude version"), "honest failure: {reason}");
                assert!(!reason.contains("below the floor"), "never a floor refusal: {reason}");
            }
            other => panic!("an unreadable version is unavailable, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn select_model_applies_on_a_session_with_turns() {
        // Offline throughout: the program name does not exist, so a stray
        // spawn would fail loudly, and the transcript below is
        // `basic.jsonl` folded through the shared stream path — a session
        // with a finished turn in it, not a fresh one. Model is
        // switchable; provider is not.
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        let path =
            format!("{}/../../fixtures/claude-code/basic.jsonl", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(path).expect("fixture reads");
        {
            let mut fold = adapter.fold.lock().expect("fold mutex");
            for line in text.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                crate::fold::step_line(&mut fold, line, &mut |_| {});
            }
        }
        let session =
            adapter.fold.lock().expect("fold mutex").session_id().expect("init seen").to_owned();
        adapter.attach_session_for_tests(&session);
        let ack = adapter
            .dispatch(Command::SelectModel {
                request_id: "r-1".into(),
                session_id: session.clone(),
                model: "sonnet".into(),
                model_provider: None,
            })
            .expect("a session with turns changes model");
        assert_eq!(ack, Ack::Accepted);
        assert_eq!(adapter.fold.lock().expect("fold mutex").model(), Some("sonnet"));
        assert_eq!(
            adapter.model.lock().expect("model mutex").as_deref(),
            Some("sonnet"),
            "the recorded model rides the next resume's --model"
        );
        // Another session's id is refused and changes nothing.
        let refused = adapter
            .dispatch(Command::SelectModel {
                request_id: "r-2".into(),
                session_id: "someone-else".into(),
                model: "opus".into(),
                model_provider: None,
            })
            .expect_err("a foreign session is refused");
        assert!(matches!(refused, ProviderError::Rejected { .. }), "got {refused:?}");
        assert_eq!(adapter.fold.lock().expect("fold mutex").model(), Some("sonnet"));
    }

    #[test]
    fn list_models_serves_the_initialize_catalog_including_fable() {
        // Offline throughout: the program name does not exist, so a stray
        // spawn would fail loudly, and the transcript below is
        // `permission.jsonl` ingested through the shared ingest path — the
        // initialize answer confirms by id exactly as live.
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        let path =
            format!("{}/../../fixtures/claude-code/permission.jsonl", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(path).expect("fixture reads");
        {
            // The fixture's catalog answer echoes `req_init_1`: track the
            // initialize request it replies to, or the id match refuses it.
            adapter.hub.track(PendingControl {
                kind: PendingControlKind::Initialize,
                request_id: "req_init_1".into(),
                wanted_model: None,
                previous_model: None,
                previous_fold_model: None,
                wanted_effort: None,
                previous_effort: None,
                sent_at: Instant::now(),
            });
            for line in text.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                adapter.ingest_child_line(line);
            }
            assert_eq!(
                adapter.fold.lock().expect("fold mutex").catalog_models().len(),
                5,
                "the initialize answer lands on its id match"
            );
        }
        // Before any answer lands the catalog is honestly unknown — never
        // the supplied fallback served as the child's own.
        let fresh = ClaudeCodeAdapter::new("claude-must-never-spawn");
        assert!(
            matches!(
                fresh.dispatch(Command::ListModels { session: None }),
                Err(ProviderError::Unavailable { .. })
            ),
            "no answer yet, no catalog"
        );
        let session =
            adapter.fold.lock().expect("fold mutex").session_id().expect("init seen").to_owned();
        adapter.attach_session_for_tests(&session);
        let ack = adapter
            .dispatch(Command::ListModels { session: Some(session.clone()) })
            .expect("the folded catalog serves");
        match ack {
            Ack::ModelCatalog { models, provider } => {
                assert_eq!(provider, "anthropic");
                let ids: Vec<&str> = models.iter().map(|row| row.id.as_str()).collect();
                assert_eq!(
                    ids,
                    ["default", "opus[1m]", "claude-fable-5-1[1m]", "sonnet", "haiku"],
                    "every row, in answer order: {ids:?}"
                );
                let fable = models
                    .iter()
                    .find(|row| row.id == "claude-fable-5-1[1m]")
                    .expect("fable serves");
                assert_eq!(fable.label, "Fable", "the label is the displayName");
                assert_eq!(fable.efforts, ["low", "medium", "high", "xhigh", "max"]);
                assert!(
                    fable.description.as_deref().is_some_and(|detail| detail
                        .contains("claude-fable-5-1")),
                    "the detail line names the resolved id: {:?}",
                    fable.description
                );
                let haiku =
                    models.iter().find(|row| row.id == "haiku").expect("haiku serves");
                assert!(haiku.efforts.is_empty(), "haiku names no effort levels");
                assert!(
                    models.iter().all(|row| !row.active),
                    "nothing picked yet, nothing marked"
                );
            }
            other => panic!("expected a catalog, got {other:?}"),
        }
        // A pick flags its row: the chip, the menus and the ack agree.
        adapter
            .dispatch(Command::SelectModel {
                request_id: "r-1".into(),
                session_id: session.clone(),
                model: "sonnet".into(),
                model_provider: None,
            })
            .expect("a pick lands");
        let ack = adapter
            .dispatch(Command::ListModels { session: Some(session.clone()) })
            .expect("the catalog serves again");
        match ack {
            Ack::ModelCatalog { models, .. } => {
                let active: Vec<&str> = models
                    .iter()
                    .filter(|row| row.active)
                    .map(|row| row.id.as_str())
                    .collect();
                assert_eq!(active, ["sonnet"], "the pick marks its row");
            }
            other => panic!("expected a catalog, got {other:?}"),
        }
    }

    #[test]
    fn select_model_writes_a_set_model_control_request() {
        // Offline: no child runs, so nothing is delivered — but the line
        // addressed to the child is recorded byte-for-byte, which is what a
        // live child receives on its stdin.
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-7");
        adapter
            .dispatch(Command::SelectModel {
                request_id: "r-1".into(),
                session_id: "sess-7".into(),
                model: "sonnet".into(),
                model_provider: None,
            })
            .expect("a pick lands");
        let lines = adapter.sent_control_lines();
        assert_eq!(lines.len(), 1, "one control line addressed: {lines:?}");
        let written: serde_json::Value =
            serde_json::from_str(&lines[0]).expect("the line encodes JSON");
        assert_eq!(
            written,
            serde_json::json!({
                "type": "control_request",
                "request_id": written.get("request_id").expect("the line mints an id"),
                "request": {"subtype": "set_model", "model": "sonnet"},
            }),
            "the live child gets set_model, never a relaunch"
        );
        // The control shapes themselves pin the probed wire bytes.
        let effort: serde_json::Value =
            serde_json::from_str(&apply_effort_control_request("req-e", "high"))
                .expect("encodes JSON");
        assert_eq!(
            effort.get("request"),
            Some(&serde_json::json!({
                "subtype": "apply_flag_settings",
                "settings": {"effortLevel": "high"},
            })),
            "effort rides apply_flag_settings, never a relaunch"
        );
        let init: serde_json::Value =
            serde_json::from_str(&initialize_control_request("req-i")).expect("encodes JSON");
        assert_eq!(
            init.get("request"),
            Some(&serde_json::json!({"subtype": "initialize"})),
            "every spawn opens with initialize"
        );
    }

    #[test]
    fn clearing_the_effort_relaunches_flagless_with_resume() {
        // Offline throughout: `resume_launch_for_effort` is pure — the
        // clearing direction of the `SubmitInput` decision (raising a level
        // rides `apply_flag_settings`, never a relaunch) — so no `claude`
        // process spawns.
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-9");
        // Same level (none anywhere): no swap, the turn goes straight in.
        assert!(
            adapter.resume_launch_for_effort("sess-9", None).is_none(),
            "no effort anywhere means no relaunch"
        );
        // Clearing back to Default while a level is in effect: `--resume
        // <id>` without `--effort`, carrying the recorded `--model` along.
        *adapter.model.lock().expect("model mutex") = Some("sonnet".into());
        *adapter.effort.lock().expect("effort mutex") = Some("high".into());
        let launch = adapter
            .resume_launch_for_effort("sess-9", None)
            .expect("clearing the effort relaunches");
        let argv = &launch.argv;
        let resume = argv.iter().position(|arg| arg == "--resume").expect("--resume");
        assert_eq!(argv.get(resume + 1).map(String::as_str), Some("sess-9"));
        assert!(
            !argv.iter().any(|arg| arg == "--effort"),
            "clearing relaunches flagless: {argv:?}"
        );
        let model = argv.iter().position(|arg| arg == "--model").expect("--model");
        assert_eq!(argv.get(model + 1).map(String::as_str), Some("sonnet"));
        assert_eq!(launch.session_id, "sess-9");
        // Once the child flies Default, staying there is a no-op: the turn
        // must not pay a relaunch every send.
        *adapter.effort.lock().expect("effort mutex") = None;
        assert!(
            adapter.resume_launch_for_effort("sess-9", None).is_none(),
            "default needs no swap"
        );
    }

    #[test]
    fn resume_seeding_marks_stored_user_echoes() {
        // Offline: plant a stored transcript under a fake home and seed
        // from it. The text echo's uuid marks; the tool-result frame (no
        // bubble) and the torn line mark nothing. Unknown workspace marks
        // nothing — honest unavailable, never a guess.
        let _guard = crate::history::tests::lock_config_env();
        let _cleared = crate::history::tests::SavedConfigDir::clear();
        let home = std::env::temp_dir().join("cc-seed-test-home");
        let workspace = std::env::temp_dir().join("cc-seed-test-work");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).expect("workspace");
        let resolved = crate::history::resolve_cwd(&workspace).expect("resolves");
        let slug = crate::history::slug_for_cwd(&resolved);
        let dir = home.join(".claude").join("projects").join(slug);
        std::fs::create_dir_all(&dir).expect("slug dir");
        std::fs::write(
            dir.join("sess-9.jsonl"),
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"Say R1\"}]},\"session_id\":\"sess-9\",\"parent_tool_use_id\":null,\"uuid\":\"u-echo-1\"}\n\
             {\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"tool_use_id\":\"toolu_1\",\"type\":\"tool_result\",\"content\":\"ok\"}]},\"session_id\":\"sess-9\",\"uuid\":\"u-result-1\"}\n\
             not json at all\n",
        )
        .expect("plant");
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn")
            .with_home(home.clone())
            .with_workspace(workspace.clone());
        assert_eq!(adapter.seed_history_echoes("sess-9"), 1, "one echo marked");
        // The marked echo renders nothing; an unmarked one still bubbles.
        let echo = frame::decode_line(
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"Say R1\"}]},\"session_id\":\"sess-9\",\"parent_tool_use_id\":null,\"uuid\":\"u-echo-1\"}",
        )
        .expect("decodes");
        let fresh = frame::decode_line(
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"Say R2\"}]},\"session_id\":\"sess-9\",\"parent_tool_use_id\":null,\"uuid\":\"u-echo-2\"}",
        )
        .expect("decodes");
        {
            let mut fold = adapter.fold.lock().expect("fold mutex");
            assert!(fold.apply(&echo).is_empty(), "seeded history never re-bubbles");
            assert_eq!(fold.apply(&fresh).len(), 1, "the new turn still bubbles");
        }
        // Missing file and unknown workspace: zero, honestly.
        assert_eq!(adapter.seed_history_echoes("no-such-session"), 0);
        // No workspace (a fresh adapter after a restart — resume carries
        // no cwd): the exact `<id>.jsonl` filename still locates the
        // session's own file, never a neighbor's.
        let bare = ClaudeCodeAdapter::new("claude-must-never-spawn").with_home(home.clone());
        assert_eq!(bare.seed_history_echoes("sess-9"), 1, "no workspace, exact id still seeds");
        assert_eq!(bare.seed_history_echoes("no-such-session"), 0);
        // But two files claiming one id refuse: an ambiguous scan is not
        // an answer.
        let other_workspace = std::env::temp_dir().join("cc-seed-test-other-work");
        let _ = std::fs::remove_dir_all(&other_workspace);
        std::fs::create_dir_all(&other_workspace).expect("other workspace");
        let other_slug = crate::history::slug_for_cwd(
            &crate::history::resolve_cwd(&other_workspace).expect("other resolves"),
        );
        let other_dir = home.join(".claude").join("projects").join(other_slug);
        std::fs::create_dir_all(&other_dir).expect("other slug dir");
        std::fs::write(other_dir.join("sess-9.jsonl"), "{}\n").expect("plant twin");
        assert_eq!(bare.seed_history_echoes("sess-9"), 0, "twins refuse, never pick one");
        // The workspace-known adapter still reads its own file: the scan
        // never overrides an exact slug path.
        assert_eq!(adapter.seed_history_echoes("sess-9"), 1, "the slug path wins over twins");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&workspace);
        let _ = std::fs::remove_dir_all(&other_workspace);
    }

    /// B12fix: stored history goes through the same `isMeta` handling as
    /// live: the loader replays the file through `frame::decode_line`, so
    /// a replayed skill body is a chip, never a user bubble, and never
    /// splits the turn — on reopen exactly as on arrival.
    #[test]
    fn stored_history_suppresses_ismeta_skill_bodies_like_live() {
        let _guard = crate::history::tests::lock_config_env();
        let _cleared = crate::history::tests::SavedConfigDir::clear();
        let home = std::env::temp_dir().join("cc-ismeta-test-home");
        let workspace = std::env::temp_dir().join("cc-ismeta-test-work");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).expect("workspace");
        let resolved = crate::history::resolve_cwd(&workspace).expect("resolves");
        let slug = crate::history::slug_for_cwd(&resolved);
        let dir = home.join(".claude").join("projects").join(slug);
        std::fs::create_dir_all(&dir).expect("slug dir");
        std::fs::write(
            dir.join("sess-ismeta.jsonl"),
            "{\"type\":\"assistant\",\"message\":{\"id\":\"msg-1\",\"content\":[{\"type\":\"tool_use\",\"id\":\"toolu-skill\",\"name\":\"Skill\",\"input\":{\"skill\":\"plan\"}}]},\"uuid\":\"u-1\",\"session_id\":\"sess-ismeta\"}\n\
             {\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"# Skill: plan\\nDo the thing.\"}]},\"uuid\":\"u-2\",\"session_id\":\"sess-ismeta\",\"isMeta\":true}\n\
             {\"type\":\"assistant\",\"message\":{\"id\":\"msg-1\",\"content\":[{\"type\":\"text\",\"text\":\"On it.\"}]},\"uuid\":\"u-3\",\"session_id\":\"sess-ismeta\"}\n",
        )
        .expect("plant");
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn")
            .with_home(home.clone())
            .with_workspace(workspace.clone());
        let (deltas, _) = adapter.stored_history("sess-ismeta").expect("stored history reads");
        let bubbles: Vec<&str> = deltas
            .iter()
            .filter_map(|delta| match delta {
                aui_protocol::Delta::TurnStarted { turn: aui_protocol::Turn::User { text, .. }, .. } => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect();
        assert!(bubbles.is_empty(), "no user bubble for the stored skill body: {bubbles:?}");
        let assistant_starts = deltas
            .iter()
            .filter(|delta| {
                matches!(delta, aui_protocol::Delta::TurnStarted { turn: aui_protocol::Turn::Assistant { .. }, .. })
            })
            .count();
        assert_eq!(assistant_starts, 1, "one assistant turn, never split: {deltas:?}");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn the_pinned_config_dir_wins_over_home_and_env() {
        use std::path::PathBuf;
        let _guard = crate::history::tests::lock_config_env();
        let pinned = PathBuf::from("/tmp/baaz-state/claude-home");
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn")
            .with_home(PathBuf::from("/tmp/owner-home"))
            .with_config_dir(pinned.clone());
        // The pin wins even with the env var set.
        let _saved =
            crate::history::tests::SavedConfigDir::set(&PathBuf::from("/tmp/env-config"));
        assert_eq!(adapter.resolved_config_dir(), pinned, "the pin wins over the env");
        // Without the pin, the env var wins over the test home.
        let unpinned =
            ClaudeCodeAdapter::new("claude-must-never-spawn").with_home(PathBuf::from("/tmp/owner-home"));
        assert_eq!(
            unpinned.resolved_config_dir(),
            PathBuf::from("/tmp/env-config"),
            "the env wins over the home"
        );
    }

    #[test]
    fn a_legacy_pin_withholds_the_home_override_for_that_session_only() {
        use std::path::PathBuf;
        let id = format!(
            "sess-env-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        );
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn")
            .with_config_dir(PathBuf::from("/tmp/baaz-state/claude-home"));
        // No pin: the Baaz home override rides.
        let env = adapter.home_env_for("some-other-session").expect("Baaz-homed env");
        assert!(
            env.iter().any(|(name, value)| name == "CLAUDE_CONFIG_DIR"
                && value == "/tmp/baaz-state/claude-home"),
            "the child writes to the Baaz home: {env:?}"
        );
        // Pinned: no override at all — the child keeps the owner's real
        // home, and history reads it too.
        crate::home::pin_legacy_resume(&id);
        assert!(
            adapter.home_env_for(&id).is_none(),
            "a legacy child carries no CLAUDE_CONFIG_DIR"
        );
        assert!(
            adapter.home_env_for("some-other-session").is_some(),
            "the pin never leaks across sessions"
        );
        crate::home::clear_legacy_resume(&id);
        assert!(adapter.home_env_for(&id).is_some(), "a cleared pin resumes Baaz-homed");
    }

    #[test]
    fn without_pin_or_env_history_reads_the_test_home_then_the_baaz_home() {
        use std::path::PathBuf;
        let _guard = crate::history::tests::lock_config_env();
        let _cleared = crate::history::tests::SavedConfigDir::clear();
        let adapter =
            ClaudeCodeAdapter::new("claude-must-never-spawn").with_home(PathBuf::from("/tmp/owner-home"));
        assert_eq!(
            adapter.resolved_config_dir(),
            PathBuf::from("/tmp/owner-home/.claude"),
            "a test home resolves through its own .claude"
        );
        let bare = ClaudeCodeAdapter::new("claude-must-never-spawn");
        assert_eq!(
            bare.resolved_config_dir(),
            crate::home::default_home(),
            "production resolves through the Baaz-owned home"
        );
    }

    #[test]
    fn a_late_created_owner_dir_is_linked_on_the_next_spawn() {
        let root = std::env::temp_dir().join(format!(
            "cc-prepare-home-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        let owner = root.join("owner-home");
        let state = root.join("state");
        // The owner has settings but no skills dir yet: adapter setup
        // links only what exists.
        std::fs::create_dir_all(owner.join(".claude")).expect("owner claude dir");
        std::fs::write(owner.join(".claude").join("settings.json"), "{}").expect("owner settings");
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn")
            .with_home(owner.clone())
            .with_config_dir(state.join("claude-home"));
        adapter.prepare_home(false);
        let home = state.join("claude-home");
        assert!(
            std::fs::symlink_metadata(home.join("settings.json")).expect("meta").file_type().is_symlink(),
            "what exists at setup is linked"
        );
        assert!(
            std::fs::symlink_metadata(home.join("skills")).is_err(),
            "an absent source links nothing"
        );
        // The owner creates skills/ later: the next spawn's re-ensure
        // picks it up without a restart.
        std::fs::create_dir_all(owner.join(".claude").join("skills")).expect("late owner skills");
        adapter.prepare_home(false);
        assert!(
            std::fs::symlink_metadata(home.join("skills")).expect("meta").file_type().is_symlink(),
            "a late-created owner dir is linked on the next spawn"
        );
        assert!(home.join("skills").is_dir(), "the linked dir reads as a dir");
        // Idempotent: a third run changes nothing.
        adapter.prepare_home(false);
        assert!(
            std::fs::symlink_metadata(home.join("skills")).expect("meta").file_type().is_symlink()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_account_is_true_after_a_meter_reading() {
        // A rate-limit reading from the live child is authenticated API
        // traffic observed first-hand: signed in, with the meter label.
        let adapter = ClaudeCodeAdapter::new("claude");
        let frame = frame::decode_line(meter_line()).expect("meter line decodes");
        adapter.fold.lock().expect("fold mutex").apply(&frame);
        let ack = adapter.dispatch(Command::ReadAccount).expect("account reads");
        match ack {
            Ack::Account { signed_in: true, label: Some(label) } => {
                assert!(label.contains("97%"), "live meter label: {label}");
            }
            other => panic!("expected signed-in account with a label, got {other:?}"),
        }
    }

    fn race_adapter_with_pending() -> std::sync::Arc<ClaudeCodeAdapter> {
        // Pure decode-and-fold: no `claude` process is ever spawned (the
        // binary name does not exist, so a stray spawn would fail loudly).
        let adapter = std::sync::Arc::new(ClaudeCodeAdapter::new("claude-must-never-spawn"));
        let frame = frame::decode_line(
            r#"{"type":"control_request","request_id":"req-race","request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"toolu_1","input":{}}}"#,
        )
        .expect("decodes");
        adapter.fold.lock().expect("fold mutex").apply(&frame);
        assert_eq!(
            adapter.fold.lock().expect("fold mutex").pending_approvals().len(),
            1,
            "one pending request to fight over"
        );
        adapter
    }

    /// THE SINGLE-DECISION TEST. Eight threads decide one id: the claim is
    /// the removal, so exactly one mints the answer line and the rest get
    /// a clean rejection — never a second `control_response` with the
    /// same `request_id`. The claim is the whole race: `decide_approval`
    /// writes exactly the line its claim minted.
    #[test]
    fn concurrent_decisions_for_one_id_claim_exactly_one_line() {
        let adapter = race_adapter_with_pending();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let adapter = std::sync::Arc::clone(&adapter);
            handles.push(std::thread::spawn(move || {
                adapter.claim_approval("req-race", "deny", None).map(|claimed| claimed.line().to_owned())
            }));
        }
        let mut lines = Vec::new();
        let mut rejected = 0;
        for handle in handles {
            match handle.join().expect("thread joins") {
                Ok(line) => lines.push(line),
                Err(error) => {
                    assert!(
                        matches!(error, ProviderError::Rejected { .. }),
                        "losers get a clean rejection, never a second line: {error}"
                    );
                    rejected += 1;
                }
            }
        }
        assert_eq!(lines.len(), 1, "exactly one decision claims the right to answer");
        assert_eq!(rejected, 7);
        let written: serde_json::Value = serde_json::from_str(&lines[0]).expect("encodes JSON");
        assert_eq!(
            written
                .get("response")
                .and_then(|response| response.get("request_id"))
                .and_then(serde_json::Value::as_str),
            Some("req-race")
        );
        assert_eq!(
            written
                .get("response")
                .and_then(|response| response.get("response"))
                .and_then(|response| response.get("behavior"))
                .and_then(serde_json::Value::as_str),
            Some("deny")
        );
        assert!(adapter.fold.lock().expect("fold mutex").pending_approvals().is_empty());
        // …and a retry afterwards still finds nothing: claimed exactly once.
        let error =
            adapter.claim_approval("req-race", "deny", None).expect_err("nothing left to claim");
        assert!(matches!(error, ProviderError::Rejected { .. }));
    }

    /// Scripted-frame helpers for the confirmation tests below: mint the
    /// child lines a live CLI would print, and pull refusal banners off
    /// the event stream.
    fn success_line(request_id: &str) -> String {
        serde_json::json!({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": request_id},
        })
        .to_string()
    }

    fn error_line(request_id: &str, reason: &str) -> String {
        serde_json::json!({
            "type": "control_response",
            "response": {"subtype": "error", "request_id": request_id, "error": reason},
        })
        .to_string()
    }

    fn catalog_line(request_id: &str, subtype: &str, error: Option<&str>) -> String {
        let mut response =
            serde_json::json!({"subtype": subtype, "request_id": request_id, "response": {
                "models": [
                    {"value": "sonnet", "displayName": "Sonnet",
                     "resolvedModel": "claude-sonnet-5",
                     "supportedEffortLevels": ["low", "high"], "supportsAutoMode": true},
                    {"value": "haiku", "displayName": "Haiku",
                     "resolvedModel": "claude-haiku-4-5-20251001"},
                ],
            }});
        if let Some(reason) = error {
            response["error"] = serde_json::Value::String(reason.to_owned());
        }
        serde_json::json!({"type": "control_response", "response": response}).to_string()
    }

    fn pending_id(adapter: &ClaudeCodeAdapter) -> String {
        adapter
            .hub
            .pending
            .lock()
            .expect("pending mutex")
            .keys()
            .next()
            .cloned()
            .expect("one tracked request")
    }

    fn drain_events(adapter: &ClaudeCodeAdapter) -> Vec<ProviderEvent> {
        let events = adapter.events();
        let mut out = Vec::new();
        while let Ok(event) = events.try_recv() {
            out.push(event);
        }
        out
    }

    fn control_cards(events: &[ProviderEvent]) -> Vec<(String, String)> {
        use aui_protocol::{Block, Delta};
        events
            .iter()
            .flat_map(|event| match event {
                ProviderEvent::Deltas { deltas, .. } => deltas.clone(),
                _ => Vec::new(),
            })
            .filter_map(|delta| match delta {
                Delta::BlockAdded { block: Block::Generic { kind, text, .. }, .. } => {
                    Some((kind, text))
                }
                _ => None,
            })
            .collect()
    }

    /// Answer one tracked `initialize` with [`catalog_line`]'s two rows:
    /// Sonnet runs in auto, Haiku (no `supportsAutoMode`) does not.
    fn fold_catalog(adapter: &ClaudeCodeAdapter) {
        adapter.hub.track(PendingControl {
            kind: PendingControlKind::Initialize,
            request_id: "init-1".into(),
            wanted_model: None,
            previous_model: None,
            previous_fold_model: None,
            wanted_effort: None,
            previous_effort: None,
            sent_at: Instant::now(),
        });
        adapter.ingest_child_line(&catalog_line("init-1", "success", None));
    }

    /// The catalog says which rows have no auto mode, and `ListModels`
    /// carries it as the row's unavailable modes; a row that has auto
    /// carries none.
    #[test]
    fn the_catalog_marks_auto_unavailable_on_a_model_without_it() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        fold_catalog(&adapter);
        let Ok(Ack::ModelCatalog { models, .. }) =
            adapter.dispatch(Command::ListModels { session: Some("sess-1".into()) })
        else {
            panic!("the catalog answers");
        };
        let limits: Vec<(&str, &[PermissionMode])> =
            models.iter().map(|row| (row.id.as_str(), row.unavailable_modes.as_slice())).collect();
        assert_eq!(limits, vec![("sonnet", &[][..]), ("haiku", &[PermissionMode::OnRequest][..])]);
    }

    /// Switching to a model without auto while in Auto asks the child into
    /// its asking mode and banners why — the CLI itself accepts the switch
    /// and keeps reporting `auto` (probed). The chip waits for the child's
    /// report. A switch onto a model with auto, or one made outside Auto,
    /// sends nothing more.
    #[test]
    fn switching_to_a_model_without_auto_leaves_auto_visibly() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        fold_catalog(&adapter);
        adapter.fold.lock().expect("fold mutex").note_permission_mode(PermissionMode::OnRequest);
        let select = |model: &str| {
            adapter
                .dispatch(Command::SelectModel {
                    request_id: "r-1".into(),
                    session_id: "sess-1".into(),
                    model: model.into(),
                    model_provider: None,
                })
                .expect("a pick lands");
        };
        select("sonnet");
        assert_eq!(adapter.sent_control_lines().len(), 1, "set_model only");
        select("haiku");
        let lines = adapter.sent_control_lines();
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[1].contains("set_model"), "{}", lines[1]);
        assert!(lines[2].contains("set_permission_mode") && lines[2].contains("\"mode\":\"manual\""), "{}", lines[2]);
        let cards = control_cards(&drain_events(&adapter));
        assert_eq!(cards.len(), 1, "{cards:?}");
        assert_eq!(cards[0].0, CONTROL_MODE_FALLBACK_CARD);
        assert!(cards[0].1.contains("Auto is not available for Haiku"), "{}", cards[0].1);
        assert_eq!(
            adapter.fold.lock().expect("fold mutex").permission_mode(),
            Some(PermissionMode::OnRequest),
            "the chip moves on the child's report, not here"
        );
        // Out of Auto, the same switch sends no mode change.
        adapter.fold.lock().expect("fold mutex").note_permission_mode(PermissionMode::PromptUnmatched);
        select("haiku");
        assert_eq!(adapter.sent_control_lines().len(), 4);
    }

    /// A `set_model` the child confirms stays picked: success finalises
    /// the optimistic record and banners nothing.
    #[test]
    fn set_model_success_confirms_the_pick() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        adapter
            .dispatch(Command::SelectModel {
                request_id: "r-1".into(),
                session_id: "sess-1".into(),
                model: "opus".into(),
                model_provider: None,
            })
            .expect("a pick lands");
        adapter.ingest_child_line(&success_line(&pending_id(&adapter)));
        assert_eq!(
            adapter.model.lock().expect("model mutex").as_deref(),
            Some("opus"),
            "success keeps the pick"
        );
        assert_eq!(
            adapter.fold.lock().expect("fold mutex").model(),
            Some("opus"),
            "the fold agrees"
        );
        assert!(adapter.hub.pending.lock().expect("pending mutex").is_empty(), "settled");
        assert!(
            control_cards(&drain_events(&adapter)).is_empty(),
            "a confirm banners nothing"
        );
    }

    /// A `set_model` the child rejects rolls back to the recorded model
    /// and banners the CLI's reason: the chip shows what is in effect.
    #[test]
    fn set_model_error_rolls_back_and_banners() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        *adapter.model.lock().expect("model mutex") = Some("sonnet".into());
        adapter.fold.lock().expect("fold mutex").set_model("sonnet");
        adapter
            .dispatch(Command::SelectModel {
                request_id: "r-1".into(),
                session_id: "sess-1".into(),
                model: "opus".into(),
                model_provider: None,
            })
            .expect("a pick lands");
        assert_eq!(
            adapter.model.lock().expect("model mutex").as_deref(),
            Some("opus"),
            "optimistic until answered"
        );
        adapter.ingest_child_line(&error_line(&pending_id(&adapter), "Model 'opus' not found"));
        assert_eq!(
            adapter.model.lock().expect("model mutex").as_deref(),
            Some("sonnet"),
            "the refusal restores the recorded model"
        );
        assert_eq!(
            adapter.fold.lock().expect("fold mutex").model(),
            Some("sonnet"),
            "the fold follows"
        );
        let cards = control_cards(&drain_events(&adapter));
        assert_eq!(cards.len(), 1, "one refusal banner: {cards:?}");
        assert_eq!(cards[0].0, CONTROL_MODEL_REJECTED_CARD);
        assert!(cards[0].1.contains("Model 'opus' not found"), "the CLI reason: {:?}", cards[0].1);
        assert!(cards[0].1.contains("sonnet"), "the model in effect: {:?}", cards[0].1);
    }

    /// A `set_model` the child never answers refuses after the timeout:
    /// silence rolls back with a clear message, never poses as applied.
    #[test]
    fn set_model_timeout_rolls_back() {
        use std::time::Duration;
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        *adapter.model.lock().expect("model mutex") = Some("sonnet".into());
        adapter.fold.lock().expect("fold mutex").set_model("sonnet");
        adapter
            .dispatch(Command::SelectModel {
                request_id: "r-1".into(),
                session_id: "sess-1".into(),
                model: "opus".into(),
                model_provider: None,
            })
            .expect("a pick lands");
        // Age the tracked request past the timeout: the next dispatch —
        // any command — settles it without waiting the clock out.
        for request in adapter.hub.pending.lock().expect("pending mutex").values_mut() {
            request.sent_at -= CONTROL_CONFIRM_TIMEOUT + Duration::from_secs(1);
        }
        let _ = adapter.dispatch(Command::ListModels { session: None });
        assert_eq!(
            adapter.model.lock().expect("model mutex").as_deref(),
            Some("sonnet"),
            "the silence restores the recorded model"
        );
        let cards = control_cards(&drain_events(&adapter));
        assert_eq!(cards.len(), 1, "one timeout banner: {cards:?}");
        assert!(cards[0].1.contains("no answer arrived within 10s"), "{:?}", cards[0].1);
    }

    /// A relaunch forgets the killed child's unanswered requests: their
    /// timeout must never refuse against the new child's state.
    #[test]
    fn relaunch_forgets_requests_the_old_child_never_answered() {
        use std::time::Duration;
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        *adapter.model.lock().expect("model mutex") = Some("sonnet".into());
        adapter
            .dispatch(Command::SelectModel {
                request_id: "r-1".into(),
                session_id: "sess-1".into(),
                model: "opus".into(),
                model_provider: None,
            })
            .expect("a pick lands");
        assert_eq!(adapter.hub.pending.lock().expect("pending mutex").len(), 1, "set_model is tracked");
        let launch = SessionLaunch {
            argv: Vec::new(),
            cwd: None,
            session_id: "sess-1".into(),
            model: Some("opus".into()),
            effort: None,
            permission_mode: None,
        };
        // The spawn fails (no such program); the old child's requests are
        // forgotten before it is attempted.
        let _ = adapter.relaunch(&launch);
        assert!(adapter.hub.pending.lock().expect("pending mutex").is_empty(), "nothing stale survives");
        for request in adapter.hub.pending.lock().expect("pending mutex").values_mut() {
            request.sent_at -= CONTROL_CONFIRM_TIMEOUT + Duration::from_secs(1);
        }
        let _ = adapter.dispatch(Command::ListModels { session: None });
        assert_eq!(adapter.model.lock().expect("model mutex").as_deref(), Some("opus"), "the new pick stands");
        assert!(control_cards(&drain_events(&adapter)).is_empty(), "no stale banner");
    }

    /// An `apply_flag_settings` the child rejects falls back to the
    /// resume relaunch carrying the level — never left unapplied — and
    /// banners the CLI's reason.
    #[test]
    fn apply_effort_error_falls_back_to_relaunch() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-9");
        *adapter.model.lock().expect("model mutex") = Some("sonnet".into());
        // No child runs, so the submit itself cannot go out — but the
        // effort arm runs first, and its tracked request is what the
        // scripted answer settles.
        let _ = adapter.dispatch(Command::SubmitInput {
            request_id: "t-1".into(),
            session_id: "sess-9".into(),
            parts: vec![provider::SubmissionPart::Text("hi".into())],
            display_text: None,
            effort: Some("high".into()),
        });
        assert_eq!(
            adapter.effort.lock().expect("effort mutex").as_deref(),
            Some("high"),
            "optimistic until answered"
        );
        adapter.ingest_child_line(&error_line(&pending_id(&adapter), "unsupported effort: high"));
        assert_eq!(
            adapter.effort.lock().expect("effort mutex").clone(),
            None,
            "the refusal restores the level in effect"
        );
        let launch =
            adapter.hub.take_effort_relaunch().expect("the refusal records a relaunch");
        assert_eq!(launch.session_id, "sess-9");
        assert!(
            launch.argv.iter().any(|arg| arg == "--resume"),
            "the fallback resumes: {:?}",
            launch.argv
        );
        let effort = launch.argv.iter().position(|arg| arg == "--effort").expect("--effort");
        assert_eq!(launch.argv.get(effort + 1).map(String::as_str), Some("high"));
        let model = launch.argv.iter().position(|arg| arg == "--model").expect("--model");
        assert_eq!(launch.argv.get(model + 1).map(String::as_str), Some("sonnet"));
        let cards = control_cards(&drain_events(&adapter));
        assert_eq!(cards.len(), 1, "one refusal banner: {cards:?}");
        assert_eq!(cards[0].0, CONTROL_EFFORT_REJECTED_CARD);
        assert!(cards[0].1.contains("unsupported effort: high"), "{:?}", cards[0].1);
    }

    /// A `control_response` matching no tracked id changes nothing: not
    /// the catalog, not the model, and no banner.
    #[test]
    fn stray_control_response_changes_nothing() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        // A confirmed catalog to defend: one tracked initialize, answered.
        adapter.hub.track(PendingControl {
            kind: PendingControlKind::Initialize,
            request_id: "init-1".into(),
            wanted_model: None,
            previous_model: None,
            previous_fold_model: None,
            wanted_effort: None,
            previous_effort: None,
            sent_at: Instant::now(),
        });
        adapter.ingest_child_line(&catalog_line("init-1", "success", None));
        assert_eq!(adapter.fold.lock().expect("fold mutex").catalog_models().len(), 2);
        adapter
            .dispatch(Command::SelectModel {
                request_id: "r-1".into(),
                session_id: "sess-1".into(),
                model: "opus".into(),
                model_provider: None,
            })
            .expect("a pick lands");
        // Foreign ids, success and error alike, with and without a catalog
        // payload: none of them may move anything.
        adapter.ingest_child_line(&catalog_line("foreign-9", "success", None));
        adapter.ingest_child_line(&error_line("foreign-10", "Model 'opus' not found"));
        assert_eq!(
            adapter.model.lock().expect("model mutex").as_deref(),
            Some("opus"),
            "the pick stands"
        );
        assert_eq!(
            adapter.fold.lock().expect("fold mutex").catalog_models().len(),
            2,
            "the catalog stands"
        );
        assert!(
            control_cards(&drain_events(&adapter)).is_empty(),
            "a stray banners nothing"
        );
        assert!(
            adapter.hub.pending.lock().expect("pending mutex").len() == 1,
            "the real request still waits"
        );
    }

    /// The initialize reply counts only on its own id and only on
    /// success: a foreign answer never populates the catalog, and an
    /// error keeps the fallback even when it carries rows.
    #[test]
    fn initialize_reply_matches_by_id() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        let track_init = |adapter: &ClaudeCodeAdapter, id: &str| {
            adapter.hub.track(PendingControl {
                kind: PendingControlKind::Initialize,
                request_id: id.into(),
                wanted_model: None,
                previous_model: None,
                previous_fold_model: None,
                wanted_effort: None,
                previous_effort: None,
                sent_at: Instant::now(),
            });
        };
        track_init(&adapter, "init-1");
        // A foreign success with rows: not ours, not kept.
        adapter.ingest_child_line(&catalog_line("foreign-1", "success", None));
        assert!(
            adapter.fold.lock().expect("fold mutex").catalog_models().is_empty(),
            "a foreign answer never populates the catalog"
        );
        // Our error, rows or not: the fallback stands, logged, no banner.
        adapter.ingest_child_line(&catalog_line("init-1", "error", Some("handshake busy")));
        assert!(
            adapter.fold.lock().expect("fold mutex").catalog_models().is_empty(),
            "an error keeps the fallback catalog"
        );
        assert!(
            control_cards(&drain_events(&adapter)).is_empty(),
            "an initialize refusal banners nothing"
        );
        // Our success: the catalog lands.
        track_init(&adapter, "init-2");
        adapter.ingest_child_line(&catalog_line("init-2", "success", None));
        assert_eq!(
            adapter.fold.lock().expect("fold mutex").catalog_models().len(),
            2,
            "the matching success restates the catalog"
        );
    }

    /// An unknown control subtype arrives, the caller answers, a response
    /// line is minted: no received request is ever unanswerable.
    #[test]
    fn unknown_control_subtype_can_be_answered() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        let frame = frame::decode_line(
            r#"{"type":"control_request","request_id":"req-x","request":{"subtype":"frobnicate"}}"#,
        )
        .expect("decodes");
        adapter.fold.lock().expect("fold mutex").apply(&frame);
        let claimed =
            adapter.claim_approval("req-x", "deny", Some("no such tool")).expect("answerable");
        let written: serde_json::Value =
            serde_json::from_str(claimed.line()).expect("encodes JSON");
        assert_eq!(
            written
                .get("response")
                .and_then(|response| response.get("request_id"))
                .and_then(serde_json::Value::as_str),
            Some("req-x")
        );
        assert_eq!(
            written
                .get("response")
                .and_then(|response| response.get("response"))
                .and_then(|response| response.get("behavior"))
                .and_then(serde_json::Value::as_str),
            Some("deny")
        );
        assert_eq!(
            written
                .get("response")
                .and_then(|response| response.get("response"))
                .and_then(|response| response.get("message"))
                .and_then(serde_json::Value::as_str),
            Some("no such tool")
        );
        let error =
            adapter.claim_approval("req-x", "deny", None).expect_err("claimed exactly once");
        assert!(matches!(error, ProviderError::Rejected { .. }));
    }

    fn mode_events(events: &[ProviderEvent]) -> Vec<(String, PermissionMode)> {
        events
            .iter()
            .filter_map(|event| match event {
                ProviderEvent::ApprovalModeChanged { session_id, mode } => {
                    Some((session_id.clone(), *mode))
                }
                _ => None,
            })
            .collect()
    }

    /// A live mode pick rides `set_permission_mode` with the CLI's own
    /// spelling, and the chip moves only on what the child reports back:
    /// the answer's `mode`, then its `status` frame — announced once.
    #[test]
    fn a_live_mode_pick_rides_set_permission_mode_and_folds_the_answer() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        adapter.fold.lock().expect("fold mutex").set_session_id("sess-1");
        adapter
            .dispatch(Command::SelectApprovalMode {
                request_id: "r-1".into(),
                session_id: "sess-1".into(),
                mode: PermissionMode::DenyUnmatched,
            })
            .expect("a pick lands");
        // Read-only's deny layer goes on first (see the layer tests below),
        // then the mode switch.
        let lines = adapter.sent_control_lines();
        assert_eq!(lines.len(), 2, "{lines:?}");
        let layer: serde_json::Value = serde_json::from_str(&lines[0]).expect("json");
        assert_eq!(layer["request"]["subtype"], "apply_flag_settings");
        let value: serde_json::Value = serde_json::from_str(&lines[1]).expect("json");
        assert_eq!(value["request"]["subtype"], "set_permission_mode");
        assert_eq!(value["request"]["mode"], "dontAsk");
        assert!(mode_events(&drain_events(&adapter)).is_empty(), "nothing moves before the answer");
        adapter.ingest_child_line(&serde_json::json!({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": layer["request_id"]},
        }).to_string());
        adapter.ingest_child_line(&serde_json::json!({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": value["request_id"], "response": {"mode": "dontAsk"}},
        }).to_string());
        adapter.ingest_child_line(
            r#"{"type":"system","subtype":"status","status":null,"permissionMode":"dontAsk","session_id":"sess-1"}"#,
        );
        assert_eq!(
            mode_events(&drain_events(&adapter)),
            vec![("sess-1".to_owned(), PermissionMode::DenyUnmatched)],
            "the reported mode, once"
        );
        assert!(adapter.hub.pending.lock().expect("pending mutex").is_empty(), "settled");
    }

    /// A refused mode pick banners the CLI's reason and moves nothing.
    #[test]
    fn a_refused_mode_pick_banners_and_moves_nothing() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        adapter
            .dispatch(Command::SelectApprovalMode {
                request_id: "r-1".into(),
                session_id: "sess-1".into(),
                mode: PermissionMode::PromptUnmatched,
            })
            .expect("a pick lands");
        adapter.ingest_child_line(&error_line(&pending_id(&adapter), "mode refused"));
        let events = drain_events(&adapter);
        assert!(mode_events(&events).is_empty(), "a refusal reports no mode");
        let cards = control_cards(&events);
        assert_eq!(cards.len(), 1, "one refusal banner: {cards:?}");
        assert_eq!(cards[0].0, CONTROL_MODE_REJECTED_CARD);
        assert!(cards[0].1.contains("mode refused"), "the CLI reason: {:?}", cards[0].1);
    }

    /// Full access cannot be switched into live by a child not launched in
    /// it (probed live), so the pick is admitted, shown, and owed as a
    /// `--resume` relaunch carrying `bypassPermissions` — no control line.
    /// A later live pick cancels the owed relaunch.
    #[test]
    fn full_access_from_a_non_bypass_child_relaunches_on_the_next_turn() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        *adapter.model.lock().expect("model mutex") = Some("sonnet".into());
        adapter
            .dispatch(Command::SelectApprovalMode {
                request_id: "r-1".into(),
                session_id: "sess-1".into(),
                mode: PermissionMode::AllowAll,
            })
            .expect("a pick lands");
        assert!(adapter.sent_control_lines().is_empty(), "no control line the child would refuse");
        assert_eq!(
            mode_events(&drain_events(&adapter)),
            vec![("sess-1".to_owned(), PermissionMode::AllowAll)]
        );
        let launch = adapter.take_mode_relaunch("sess-1").expect("a relaunch is owed");
        let at = launch.argv.iter().position(|arg| arg == "--permission-mode").expect("mode flag");
        assert_eq!(launch.argv[at + 1], "bypassPermissions");
        assert!(launch.argv.contains(&"--resume".to_owned()));
        assert_eq!(launch.model.as_deref(), Some("sonnet"), "the model rides along");
        assert!(adapter.take_mode_relaunch("sess-1").is_none(), "the take is the claim");

        adapter
            .dispatch(Command::SelectApprovalMode {
                request_id: "r-2".into(),
                session_id: "sess-1".into(),
                mode: PermissionMode::AllowAll,
            })
            .expect("a pick lands");
        adapter
            .dispatch(Command::SelectApprovalMode {
                request_id: "r-3".into(),
                session_id: "sess-1".into(),
                mode: PermissionMode::OnRequest,
            })
            .expect("a pick lands");
        assert!(adapter.take_mode_relaunch("sess-1").is_none(), "a live pick cancels it");
        assert_eq!(adapter.sent_control_lines().len(), 1, "the live pick went out");
    }

    /// A child launched in Full access may switch live in and out of it:
    /// the launch announces its mode, and the pick rides the control line.
    #[test]
    fn a_bypass_launch_announces_its_mode_and_switches_live() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        let launch = argv::with_permission_mode(
            adapter.launch_for_open("sess-1", None, None).expect("open launch"),
            Some(PermissionMode::AllowAll),
        );
        adapter.note_launch_mode(&launch);
        adapter.attach_session_for_tests("sess-1");
        assert_eq!(
            mode_events(&drain_events(&adapter)),
            vec![("sess-1".to_owned(), PermissionMode::AllowAll)],
            "the launched mode shows until the child reports"
        );
        // The `initialize` answer restating the same mode moves nothing.
        adapter.ingest_child_line(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"x","response":{"current_permission_mode":"bypassPermissions"}}}"#,
        );
        assert!(mode_events(&drain_events(&adapter)).is_empty(), "no move, no event");
        adapter
            .dispatch(Command::SelectApprovalMode {
                request_id: "r-1".into(),
                session_id: "sess-1".into(),
                mode: PermissionMode::AllowAll,
            })
            .expect("a pick lands");
        assert_eq!(adapter.sent_control_lines().len(), 1, "a bypass child takes it live");
        assert!(adapter.take_mode_relaunch("sess-1").is_none());
    }

    /// The `apply_flag_settings` payload of one sent control line, when it
    /// is one.
    fn applied_settings(line: &str) -> Option<serde_json::Value> {
        let value: serde_json::Value = serde_json::from_str(line).expect("json");
        (value["request"]["subtype"] == "apply_flag_settings").then(|| value["request"]["settings"].clone())
    }

    fn pick(adapter: &ClaudeCodeAdapter, mode: PermissionMode) {
        adapter
            .dispatch(Command::SelectApprovalMode {
                request_id: "r-pick".into(),
                session_id: "sess-1".into(),
                mode,
            })
            .expect("a pick lands");
    }

    /// Read-only picked live puts the deny layer on with
    /// `apply_flag_settings` before the `dontAsk` switch, and a pick away
    /// takes exactly that layer back off live — `null` per key, the probed
    /// clear — before switching. Re-picking Read-only while it holds sends
    /// no second layer.
    #[test]
    fn live_read_only_applies_the_deny_layer_and_leaving_clears_it() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        pick(&adapter, PermissionMode::DenyUnmatched);
        let lines = adapter.sent_control_lines();
        assert_eq!(applied_settings(&lines[0]), Some(argv::read_only_settings()));
        assert!(lines[1].contains("set_permission_mode"));
        pick(&adapter, PermissionMode::DenyUnmatched);
        let lines = adapter.sent_control_lines();
        assert_eq!(lines.len(), 3, "no second layer: {lines:?}");
        assert!(lines[2].contains("set_permission_mode"));
        pick(&adapter, PermissionMode::PromptUnmatched);
        let lines = adapter.sent_control_lines();
        assert_eq!(applied_settings(&lines[3]), Some(argv::read_only_settings_cleared()));
        assert!(lines[4].contains("\"mode\":\"manual\""), "{}", lines[4]);
        assert!(adapter.take_mode_relaunch("sess-1").is_none(), "a live layer leaves live");
        // Off now: another non-Read-only pick sends no clear.
        pick(&adapter, PermissionMode::OnRequest);
        let lines = adapter.sent_control_lines();
        assert_eq!(lines.len(), 6, "{lines:?}");
        assert!(applied_settings(&lines[5]).is_none());
    }

    /// A child launched Read-only carries its layer on `--settings`, which
    /// `apply_flag_settings` cannot remove (probed): a pick away is
    /// admitted and shown now, and owed as a `--resume` relaunch whose argv
    /// has the new mode and no layer. Picking Read-only back before the
    /// next turn cancels it and sends no second layer.
    #[test]
    fn leaving_a_launched_read_only_relaunches_without_the_layer() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        let launch = argv::with_permission_mode(
            adapter.launch_for_open("sess-1", None, None).expect("open launch"),
            Some(PermissionMode::DenyUnmatched),
        );
        assert!(argv::carries_read_only_settings(&launch));
        adapter.note_launch_mode(&launch);
        adapter.attach_session_for_tests("sess-1");
        let _ = drain_events(&adapter);
        pick(&adapter, PermissionMode::OnRequest);
        assert!(adapter.sent_control_lines().is_empty(), "nothing the child could honour live");
        assert_eq!(mode_events(&drain_events(&adapter)), vec![("sess-1".to_owned(), PermissionMode::OnRequest)]);
        let relaunch = adapter.take_mode_relaunch("sess-1").expect("a relaunch is owed");
        assert!(!argv::carries_read_only_settings(&relaunch), "{:?}", relaunch.argv);
        let at = relaunch.argv.iter().position(|arg| arg == "--permission-mode").expect("mode flag");
        assert_eq!(relaunch.argv[at + 1], "auto");

        pick(&adapter, PermissionMode::OnRequest);
        pick(&adapter, PermissionMode::DenyUnmatched);
        assert!(adapter.take_mode_relaunch("sess-1").is_none(), "back to Read-only cancels it");
        let lines = adapter.sent_control_lines();
        assert_eq!(lines.len(), 1, "the launched layer still holds: {lines:?}");
        assert!(lines[0].contains("set_permission_mode"));
    }

    /// Every relaunch of a Read-only session carries the layer again: a
    /// launch built with no mode (an effort swap, a revive, a refused-layer
    /// fallback) takes the session's mode, and with it `--settings`; the
    /// relaunched child then counts as launched Read-only.
    #[test]
    fn a_read_only_relaunch_keeps_the_deny_layer() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        adapter.fold.lock().expect("fold mutex").note_permission_mode(PermissionMode::DenyUnmatched);
        let relaunch = adapter.with_session_mode(argv::argv_for_resume("sess-1", None, None, Some("high")));
        assert!(argv::carries_read_only_settings(&relaunch), "{:?}", relaunch.argv);
        adapter.note_launch_mode(&relaunch);
        assert_eq!(*adapter.read_only_layer.lock().expect("read-only mutex"), ReadOnlyLayer::Launched);
        // Another mode's relaunch drops it, and the child holds none.
        adapter.fold.lock().expect("fold mutex").note_permission_mode(PermissionMode::PromptUnmatched);
        let relaunch = adapter.with_session_mode(argv::argv_for_resume("sess-1", None, None, None));
        assert!(!argv::carries_read_only_settings(&relaunch));
        adapter.note_launch_mode(&relaunch);
        assert_eq!(*adapter.read_only_layer.lock().expect("read-only mutex"), ReadOnlyLayer::Off);
    }

    /// A refused layer never leaves Read-only quietly unenforced: the
    /// refusal banners and records a resume relaunch, which takes the
    /// session's mode — Read-only, so `--settings` rides it.
    #[test]
    fn a_refused_read_only_layer_banners_and_owes_a_relaunch_carrying_it() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        adapter.fold.lock().expect("fold mutex").set_session_id("sess-1");
        pick(&adapter, PermissionMode::DenyUnmatched);
        let lines = adapter.sent_control_lines();
        let layer: serde_json::Value = serde_json::from_str(&lines[0]).expect("json");
        let switch: serde_json::Value = serde_json::from_str(&lines[1]).expect("json");
        adapter.ingest_child_line(&error_line(layer["request_id"].as_str().expect("id"), "flag settings refused"));
        adapter.ingest_child_line(&serde_json::json!({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": switch["request_id"], "response": {"mode": "dontAsk"}},
        }).to_string());
        let cards = control_cards(&drain_events(&adapter));
        assert_eq!(cards.len(), 1, "{cards:?}");
        assert_eq!(cards[0].0, CONTROL_MODE_REJECTED_CARD);
        assert!(cards[0].1.contains("flag settings refused"), "{:?}", cards[0].1);
        let owed = adapter.hub.take_effort_relaunch().expect("a relaunch is owed");
        assert!(owed.argv.contains(&"--resume".to_owned()));
        let relaunch = adapter.with_session_mode(owed);
        assert!(argv::carries_read_only_settings(&relaunch), "{:?}", relaunch.argv);
    }

    /// The `init` frame's `permissionMode` is the chip's truth: a child the
    /// CLI put in `default` reads as Ask, never the seam's default Auto.
    #[test]
    fn the_init_frame_folds_its_permission_mode() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        let init = r#"{"type":"system","subtype":"init","session_id":"sess-1","model":"sonnet","cwd":"/w","tools":[],"permissionMode":"default"}"#;
        adapter.ingest_child_line(init);
        adapter.ingest_child_line(init);
        assert_eq!(
            mode_events(&drain_events(&adapter)),
            vec![("sess-1".to_owned(), PermissionMode::PromptUnmatched)],
            "once, as Ask"
        );
    }

    /// The 2026-10-15 shape, offline: a turn is mid-tool with an approval
    /// carded when the child's stream ends. The turn must not hang — it
    /// carries one `CHILD_EXITED_CARD`, the approval card settles and
    /// leaves the answerable queue, the turn finishes, and the lane hears
    /// the connection is lost — in that order.
    #[test]
    fn a_stream_ending_mid_turn_cards_finishes_and_reports_the_loss() {
        use aui_protocol::{ApprovalState, Block, Delta};
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        let epoch = adapter.hub.begin_child();
        adapter.ingest_child_line(
            r#"{"type":"system","subtype":"init","session_id":"sess-1","model":"claude-sonnet-5","cwd":"/w","tools":[]}"#,
        );
        adapter.ingest_child_line(
            r#"{"type":"assistant","message":{"id":"msg_1","role":"assistant","model":"claude-sonnet-5","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}}]},"parent_tool_use_id":null,"session_id":"sess-1"}"#,
        );
        adapter.ingest_child_line(
            r#"{"type":"control_request","request_id":"req-1","request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"toolu_1","input":{"command":"ls"}}}"#,
        );
        assert_eq!(adapter.fold.lock().expect("fold mutex").pending_approvals().len(), 1);
        let _ = drain_events(&adapter);

        adapter.hub.ingest_end_of_stream(epoch, crate::child::CHILD_EXITED_REASON);
        let events = drain_events(&adapter);
        let deltas: Vec<Delta> = events
            .iter()
            .flat_map(|event| match event {
                ProviderEvent::Deltas { deltas, .. } => deltas.clone(),
                _ => Vec::new(),
            })
            .collect();
        let card = deltas.iter().position(|delta| {
            matches!(delta, Delta::BlockAdded { block: Block::Generic { kind, status, text }, .. }
                if kind == CHILD_EXITED_CARD && status == "error" && text == crate::child::CHILD_EXITED_REASON)
        });
        let finished = deltas.iter().position(|delta| matches!(delta, Delta::TurnFinished { .. }));
        let (card, finished) = (card.expect("the exit is carded"), finished.expect("the turn finishes"));
        assert!(card < finished, "the card lands on the turn before it finishes: {deltas:?}");
        assert!(
            deltas.iter().any(|delta| matches!(
                delta,
                Delta::BlockUpdated { block: Block::Approval { state: ApprovalState::Denied, .. }, .. }
            )),
            "the unanswerable approval card settles: {deltas:?}"
        );
        assert!(
            adapter.fold.lock().expect("fold mutex").pending_approvals().is_empty(),
            "nothing is left to answer the dead child's request"
        );
        assert!(
            matches!(events.last(), Some(ProviderEvent::ConnectionLost { reason }) if reason == crate::child::CHILD_EXITED_REASON),
            "the loss is reported last: {events:?}"
        );
        // A second end of stream (a stray) says nothing new.
        adapter.hub.ingest_end_of_stream(epoch, "again");
        assert!(control_cards(&drain_events(&adapter)).is_empty(), "no second card");
    }

    /// An idle child's stream ending cards nothing (no turn was cut off),
    /// but still reports the lost connection.
    #[test]
    fn an_idle_stream_ending_reports_the_loss_without_a_card() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        let epoch = adapter.hub.begin_child();
        adapter.hub.ingest_end_of_stream(epoch, "gone");
        let events = drain_events(&adapter);
        assert!(control_cards(&events).is_empty(), "no turn, no card: {events:?}");
        assert!(matches!(events.as_slice(), [ProviderEvent::ConnectionLost { .. }]), "{events:?}");
    }

    /// A deliberate hang-up (relaunch, close, quit) retires its child: that
    /// child's end of stream — and a replaced child's, arriving after its
    /// successor started — is expected, so it neither cards nor reports a
    /// loss that would tear down the successor.
    #[test]
    fn a_retired_or_replaced_childs_stream_end_is_ignored() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        let first = adapter.hub.begin_child();
        adapter.hub.retire_child(first);
        adapter.hub.ingest_end_of_stream(first, "hung up on purpose");
        assert!(drain_events(&adapter).is_empty(), "a retired child's end is silent");

        let old = adapter.hub.begin_child();
        let _new = adapter.hub.begin_child();
        adapter.hub.ingest_end_of_stream(old, "the predecessor ended late");
        assert!(drain_events(&adapter).is_empty(), "a replaced child's end is silent");
        // Retiring the replaced epoch must not retire its successor.
        adapter.hub.retire_child(old);
        adapter.hub.ingest_end_of_stream(_new, "the successor ended");
        assert!(
            matches!(drain_events(&adapter).as_slice(), [ProviderEvent::ConnectionLost { .. }]),
            "the current child's end still reports"
        );
    }

    /// A replaced child's late lines never reach the fold: a status frame
    /// from the old child naming its old mode would otherwise move the
    /// chip back. The current child's same frame still folds.
    #[test]
    fn a_retired_or_replaced_childs_late_lines_are_ignored() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        let status = |mode: &str| {
            format!(
                r#"{{"type":"system","subtype":"status","status":null,"permissionMode":"{mode}","session_id":"sess-1"}}"#
            )
        };
        let old = adapter.hub.begin_child();
        adapter.hub.retire_child(old);
        let new = adapter.hub.begin_child();
        adapter.hub.ingest_line_of(old, &status("dontAsk"));
        assert!(mode_events(&drain_events(&adapter)).is_empty(), "the retired child's frame is dropped");
        adapter.hub.ingest_line_of(new, &status("dontAsk"));
        assert_eq!(
            mode_events(&drain_events(&adapter)),
            vec![("sess-1".to_owned(), PermissionMode::DenyUnmatched)],
            "the current child's frame folds"
        );
    }

    /// After a lost child, the held session resumes on the next turn: the
    /// revive launch is a `--resume` carrying the recorded model and
    /// effort. A session the adapter does not hold revives nothing.
    #[test]
    fn a_lost_child_revives_with_resume_on_the_next_turn() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        *adapter.model.lock().expect("model mutex") = Some("sonnet".into());
        *adapter.effort.lock().expect("effort mutex") = Some("high".into());
        assert!(adapter.revive_launch("sess-1").is_none(), "never spawned, never lost");
        let epoch = adapter.hub.begin_child();
        adapter.hub.ingest_end_of_stream(epoch, "gone");
        let launch = adapter.revive_launch("sess-1").expect("a lost child: a revive is due");
        let at = launch.argv.iter().position(|arg| arg == "--resume").expect("resume flag");
        assert_eq!(launch.argv[at + 1], "sess-1");
        assert_eq!(launch.model.as_deref(), Some("sonnet"));
        assert_eq!(launch.effort.as_deref(), Some("high"));
        assert!(adapter.revive_launch("sess-2").is_none(), "not this adapter's session");
        // The submit attempts the revive and fails loudly (the program does
        // not exist), never writing into a dead stdin.
        let error = adapter
            .dispatch(Command::SubmitInput {
                request_id: "r-1".into(),
                session_id: "sess-1".into(),
                parts: vec![provider::SubmissionPart::Text("again".into())],
                display_text: None,
                effort: Some("high".into()),
            })
            .expect_err("the revive cannot spawn here");
        assert!(
            matches!(&error, ProviderError::Unavailable { reason } if reason.contains("respawn")),
            "the revive was attempted: {error:?}"
        );
    }
}
