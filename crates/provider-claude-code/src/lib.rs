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
pub mod hook;
pub mod auth_status;
pub mod caps;
pub mod child;
pub mod controls;
pub mod fold;
pub mod frame;
pub mod history;
pub mod home;
pub mod registry;
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
    CONTROL_MODE_FALLBACK_CARD, CONTROL_MODE_REJECTED_CARD, MCP_LAYER_HOLD, MCP_WILDCARD_DENY,
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
    /// is Default: no override in effect. Clearing back to Default rides
    /// the same channel — `apply_flag_settings{effortLevel: null}` (probed
    /// live 25 Nov 2026, docs/85-evidence/10.3: `get_settings` drops the
    /// key, same pid) — unless the level came from the launch's `--effort`
    /// flag ([`Self::launch_effort`]), which no settings layer reaches: that
    /// one keeps the `--resume` relaunch ([`Self::resume_launch_for_effort`]).
    /// Shared with the [`ControlHub`], which rolls it back when the child
    /// refuses.
    effort: Arc<Mutex<Option<String>>>,
    /// The running child's `--effort` launch flag (`None`: it launched
    /// without one). `--effort` is not a settings layer (`get_settings`
    /// never reports it, probed 25 Nov 2026), so a level that came from it
    /// clears only by relaunching.
    launch_effort: Mutex<Option<String>>,
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
    /// The approval mode Baaz last asked for — a launch's mode flag or a
    /// pick — whether or not the child has reported it yet. A relaunch
    /// built before any report keeps this mode (and so Read-only keeps its
    /// deny layer) instead of falling back to the CLI's default.
    requested_mode: Mutex<Option<PermissionMode>>,
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
    /// The session's `OpenSession.env` (or `ResumeSession.env`): every
    /// child this lane spawns (the open or resume, and each relaunch)
    /// starts with it.
    session_env: Mutex<Vec<(String, String)>>,
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
    /// connectors: the Settings › MCP "Use my own MCP servers"
    /// switch (`Layout.use_own_mcp.claude_code`), set by the host before
    /// the session commands run. Off means `--strict-mcp-config` on every
    /// launch (the default); on means the flag is stripped while the
    /// bridge config still rides when a relay is set, and the person's own
    /// servers ride beside it ([`Self::set_owner_mcp_source`]). Applies to
    /// sessions started after the change.
    use_own_mcp: Mutex<bool>,
    /// The account bills by API key (docs/85 10.6): its children ask for
    /// the one-hour prompt cache a subscription already has.
    bills_by_key: Mutex<bool>,
    /// The person's `~/.claude.json`, read (never written) at each launch
    /// while [`Self::use_own_mcp`] is on: Claude Code runs under Baaz's own
    /// `CLAUDE_CONFIG_DIR`, whose `.claude.json` is not that file, so the
    /// servers `claude mcp add` put there reach the session only as a
    /// second `--mcp-config` (docs/82 8.9). `None` (tests, and any adapter
    /// the factory did not set up) reads nothing.
    owner_mcp_source: Mutex<Option<PathBuf>>,
    /// How long Stop waits for the child to close the turn after the
    /// `interrupt` control request before ending the child instead
    /// ([`INTERRUPT_GRACE`]; tests shorten it).
    interrupt_grace: std::time::Duration,
}

/// How long Stop waits for the child to close its turn after the
/// `interrupt` control request. Past it, Stop ends the child, marks the turn
/// interrupted and leaves the session to resume on the next send: Stop
/// always stops, whether or not this CLI honours the request (docs/82 0.8).
pub const INTERRUPT_GRACE: std::time::Duration = std::time::Duration::from_millis(1500);

/// `interrupt`, the host→child control request that stops the running
/// turn (the CLI's documented stream-json control protocol; the child
/// answers `control_response` success and ends the turn with a `result`).
/// Written from the protocol, not yet confirmed live on this machine.
pub fn interrupt_control_request(request_id: &str) -> String {
    serde_json::json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {"subtype": "interrupt"},
    })
    .to_string()
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
/// turn's session jsonl carried `"effort":"high"`. The reset is
/// [`reset_effort_control_request`].
pub fn apply_effort_control_request(request_id: &str, level: &str) -> String {
    serde_json::json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {"subtype": "apply_flag_settings", "settings": {"effortLevel": level}},
    })
    .to_string()
}

/// `apply_flag_settings{effortLevel: null}`: back to Default on the live
/// child. Probed live 25 Nov 2026 (claude 2.1.276, docs/85-evidence/10.3):
/// after it `get_settings` no longer names `effortLevel`, the pid is the
/// same, and the next turn runs.
pub fn reset_effort_control_request(request_id: &str) -> String {
    serde_json::json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {"subtype": "apply_flag_settings", "settings": {"effortLevel": null}},
    })
    .to_string()
}

/// `set_permission_mode` into the CLI's own `plan` mode, or — `on` false —
/// back to `back` (docs/82 5.5). `plan` is one of the modes `claude --help`
/// (2.1.276) lists for `--permission-mode`; the live switch into it is the
/// same request the other modes take.
pub fn set_plan_mode_control_request(request_id: &str, on: bool, back: PermissionMode) -> String {
    let mode = if on { PLAN_PERMISSION_MODE } else { argv::claude_permission_mode(back) };
    serde_json::json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {"subtype": "set_permission_mode", "mode": mode},
    })
    .to_string()
}

/// The CLI's spelling of its plan permission mode.
pub const PLAN_PERMISSION_MODE: &str = "plan";

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
/// clear, while the owner's own rules stand throughout). `mcp_denies`
/// ride the layer going on ([`argv::read_only_settings_with`]).
pub fn apply_read_only_control_request(request_id: &str, on: bool, mcp_denies: &[String]) -> String {
    let settings = if on { argv::read_only_settings_with(mcp_denies) } else { argv::read_only_settings_cleared() };
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
            launch_effort: Mutex::new(None),
            bypass_capable: Mutex::new(false),
            read_only_layer: Mutex::new(ReadOnlyLayer::Off),
            mode_relaunch_due: Mutex::new(None),
            requested_mode: Mutex::new(None),
            hub,
            control_seq: Mutex::new(1),
            sent_control: Mutex::new(Vec::new()),
            workspace: Mutex::new(None),
            session_env: Mutex::new(Vec::new()),
            home_override: None,
            config_override: Mutex::new(None),
            connected: Mutex::new(false),
            version: Mutex::new(None),
            terminal: Mutex::new(None),
            use_own_mcp: Mutex::new(false),
            bills_by_key: Mutex::new(false),
            owner_mcp_source: Mutex::new(None),
            interrupt_grace: INTERRUPT_GRACE,
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

    /// Whether the account bills by API key, as the app's last sign-in
    /// probe read it (docs/85 10.6).
    pub fn set_bills_by_key(&self, bills_by_key: bool) {
        *self.bills_by_key.lock().expect("bills mutex") = bills_by_key;
    }

    /// Let the session inherit the owner's own MCP servers and
    /// connectors: the Settings › MCP "Use my own MCP servers"
    /// switch. When on, launches strip `--strict-mcp-config` (the bridge
    /// config still rides when a relay is set) and carry the person's own
    /// servers ([`Self::set_owner_mcp_source`]). Off by default.
    pub fn set_use_own_mcp(&self, use_own: bool) {
        *self.use_own_mcp.lock().expect("use-own-mcp mutex") = use_own;
    }

    /// Where the person's own servers are read from while the opt-in is
    /// on: their `~/.claude.json` (the factory passes it; read only, never
    /// written). `None` reads nothing.
    pub fn set_owner_mcp_source(&self, path: Option<PathBuf>) {
        *self.owner_mcp_source.lock().expect("owner-mcp mutex") = path;
    }

    /// Apply the opt-in to a built launch: when the switch is on, strip
    /// `--strict-mcp-config` and add the person's own servers
    /// ([`Self::with_own_mcp`]); else leave the launch as built (strict on
    /// every launch — see [`argv`]).
    fn apply_use_own_mcp(&self, launch: SessionLaunch) -> SessionLaunch {
        if *self.use_own_mcp.lock().expect("use-own-mcp mutex") {
            self.with_own_mcp(argv::without_strict_mcp(launch))
        } else {
            launch
        }
    }

    /// Add the person's own servers to an opted-in `launch` as a second
    /// `--mcp-config` (docs/82 8.9): user scope plus the launch folder's
    /// local scope from their `~/.claude.json`
    /// ([`terminal::own_mcp_servers`]), written fresh beside the bridge's
    /// config (the relay's directory, else `mcp/` in Baaz's Claude home).
    /// Nothing to add — no source, an unreadable file, no servers, nowhere
    /// to write — leaves the launch as it is; a launch already carrying
    /// the file keeps it once. Best-effort: a failed write is said on
    /// stderr and the session still starts.
    fn with_own_mcp(&self, mut launch: SessionLaunch) -> SessionLaunch {
        let Some(source) = self.owner_mcp_source.lock().expect("owner-mcp mutex").clone() else {
            return launch;
        };
        let dir = match self.terminal.lock().expect("terminal mutex").as_ref() {
            Some(relay) => Some(relay.config_dir.clone()),
            None => self.config_override.lock().expect("config mutex").as_ref().map(|home| home.join("mcp")),
        };
        let Some(dir) = dir else { return launch };
        let path = terminal::own_mcp_config_path(&dir, &launch.session_id);
        let path_text = path.to_string_lossy().into_owned();
        if launch.argv.contains(&path_text) {
            return launch;
        }
        let Ok(text) = std::fs::read_to_string(&source) else { return launch };
        let workspace = launch
            .cwd
            .clone()
            .or_else(|| self.workspace.lock().expect("workspace mutex").as_ref().map(|path| path.to_string_lossy().into_owned()));
        let servers = terminal::own_mcp_servers(&text, workspace.as_deref());
        if servers.is_empty() {
            return launch;
        }
        match terminal::write_own_mcp_config(&dir, &launch.session_id, &servers) {
            Ok(path) => {
                launch.argv.push("--mcp-config".into());
                launch.argv.push(path.to_string_lossy().into_owned());
            }
            Err(error) => eprintln!("baaz: could not pass your own MCP servers to Claude Code: {error}"),
        }
        launch
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
    /// gains (or, opted in, loses) `--strict-mcp-config` and, opted in,
    /// the person's own servers' file, so a swap never silently widens or
    /// narrows what the session may see. The person's file is not the
    /// bridge's: a launch carrying only it still gains the bridge.
    fn with_mcp_config(&self, mut launch: SessionLaunch) -> Result<SessionLaunch, ProviderError> {
        let use_own = *self.use_own_mcp.lock().expect("use-own-mcp mutex");
        let has_bridge = launch.argv.windows(2).any(|pair| pair[0] == "--mcp-config" && !pair[1].ends_with("-own.json"));
        if !has_bridge {
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
        if use_own {
            launch = self.with_own_mcp(launch);
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
        // docs/85 16.2: `skills/` is one link per skill, the owner's own and
        // the shared `~/.agents/skills`, re-made on every spawn.
        if let Some(owner_home) = owner_root.parent() {
            let _ = crate::home::ensure_skills_dir(&home, &crate::home::skill_sources(owner_home));
        }
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
            wanted_mcp: None,
            sent_at: Instant::now(),
        };
        let _ = self.send_control(&initialize_control_request(&request_id), pending);
        // The plan's limits before the first turn (docs/85 16.1): the
        // `rate_limit_event` only comes with a turn.
        let usage_id = self.next_control_id();
        let line = serde_json::json!({
            "type": "control_request",
            "request_id": usage_id,
            "request": {"subtype": "get_usage", "skip_behaviors": true},
        })
        .to_string();
        let _ = self.send_control(&line, PendingControl::bare(PendingControlKind::GetUsage, usage_id));
        // The owner's MCP servers, for the Read-only layer's MCP denies.
        self.hub.query_mcp_status();
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
            wanted_mcp: None,
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
    ///
    /// Runs on the child's CONFIRMED switch (see
    /// [`ControlHub::take_confirmed_model`]), never on the pick: a refused
    /// `set_model` leaves the session on a model that has Auto. A pick
    /// into Auto still waiting on its relaunch (out of a launched
    /// Read-only) becomes a relaunch into Ask instead — the chip moves
    /// with it, as it moved for the pick.
    ///
    /// A mode other than Auto requested since (the host falls back to Ask
    /// on the pick itself, before the child confirms) stands: nothing more
    /// is sent and no second banner shows.
    fn leave_auto_if_unsupported(&self, model: &str) {
        let requested = *self.requested_mode.lock().expect("requested mode mutex");
        if requested.is_some_and(|mode| mode != PermissionMode::OnRequest) {
            return;
        }
        let (in_auto, row) = {
            let fold = self.fold.lock().expect("fold mutex");
            (fold.permission_mode() == Some(PermissionMode::OnRequest), fold.catalog_row(model).cloned())
        };
        if !in_auto || !argv::model_lacks_auto(model, row.as_ref()) {
            return;
        }
        let text = auto_fallback_text(model, row.as_ref());
        let converted = {
            let mut due = self.mode_relaunch_due.lock().expect("mode relaunch mutex");
            let auto_due = *due == Some(PermissionMode::OnRequest);
            if auto_due {
                *due = Some(PermissionMode::PromptUnmatched);
            }
            auto_due
        };
        if converted {
            let session_id = self.session_id.lock().expect("session mutex").clone();
            let moved = self.fold.lock().expect("fold mutex").note_permission_mode(PermissionMode::PromptUnmatched);
            if let (Some(session_id), true) = (session_id, moved) {
                let _ = self.tx.send(ProviderEvent::ApprovalModeChanged {
                    session_id,
                    mode: PermissionMode::PromptUnmatched,
                });
            }
            self.hub.emit_card(CONTROL_MODE_FALLBACK_CARD, text);
            return;
        }
        if self.send_set_permission_mode(PermissionMode::PromptUnmatched).is_ok() {
            self.hub.emit_card(CONTROL_MODE_FALLBACK_CARD, text);
        }
    }

    /// A launch asking for Auto on a model that cannot run it (see
    /// [`argv::model_lacks_auto`]) launches in Ask instead — the mode the
    /// switch onto that model would have fallen back to live — and hands
    /// back the notice saying so, emitted once the child is noted. The
    /// effective model is the launch's `--model`, else the session's
    /// recorded pick. Every other launch is returned as built. Pure apart
    /// from recording Ask as the requested mode.
    fn launch_off_unsupported_auto(&self, launch: SessionLaunch) -> (SessionLaunch, Option<String>) {
        if launch.permission_mode != Some(PermissionMode::OnRequest) {
            return (launch, None);
        }
        let Some(model) = launch.model.clone().or_else(|| self.model.lock().expect("model mutex").clone()) else {
            return (launch, None);
        };
        let text = {
            let fold = self.fold.lock().expect("fold mutex");
            let row = fold.catalog_row(&model);
            argv::model_lacks_auto(&model, row).then(|| auto_fallback_text(&model, row))
        };
        let Some(text) = text else { return (launch, None) };
        *self.requested_mode.lock().expect("requested mode mutex") = Some(PermissionMode::PromptUnmatched);
        (argv::with_permission_mode(launch, Some(PermissionMode::PromptUnmatched)), Some(text))
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
            wanted_mcp: None,
            sent_at: Instant::now(),
        };
        self.send_control(&apply_effort_control_request(&request_id, level), pending)
    }

    /// The `apply_flag_settings{effortLevel: null}` line taking a live-raised
    /// effort back to Default from the next turn, with no relaunch (docs/85
    /// 10.3). A refusal rolls the record back and owes a resume relaunch
    /// without `--effort`, like a refused raise.
    fn send_reset_effort(&self, previous_effort: Option<String>) -> Result<(), ProviderError> {
        let request_id = self.next_control_id();
        let pending = PendingControl {
            kind: PendingControlKind::ApplyEffort,
            request_id: request_id.clone(),
            wanted_model: None,
            previous_model: None,
            previous_fold_model: None,
            wanted_effort: None,
            previous_effort,
            wanted_mcp: None,
            sent_at: Instant::now(),
        };
        self.send_control(&reset_effort_control_request(&request_id), pending)
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
            wanted_mcp: None,
            sent_at: Instant::now(),
        };
        self.send_control(&set_permission_mode_control_request(&request_id, mode), pending)
    }

    /// The `set_permission_mode` line turning plan mode on (`plan`) or off
    /// (back to `back`, the mode the session was in).
    fn send_set_plan_mode(&self, on: bool, back: PermissionMode) -> Result<(), ProviderError> {
        let request_id = self.next_control_id();
        let pending = PendingControl {
            kind: PendingControlKind::SetPlanMode(on),
            request_id: request_id.clone(),
            wanted_model: None,
            previous_model: None,
            previous_fold_model: None,
            wanted_effort: None,
            previous_effort: None,
            wanted_mcp: None,
            sent_at: Instant::now(),
        };
        self.send_control(&set_plan_mode_control_request(&request_id, on, back), pending)
    }

    /// The `apply_flag_settings` line putting the Read-only deny layer on
    /// the live child (`wanted` is Read-only) or taking it off (`wanted` is
    /// the mode being left for). A refusal banners and records a resume
    /// relaunch, which carries the layer exactly when the mode does.
    ///
    /// The layer state and the line move as one step in the hub
    /// ([`ControlHub::send_read_only_layer`]), so the pump's MCP refresh
    /// can never interleave between them. Going on, the owner's own acting
    /// MCP tools ride as last known — or every MCP tool
    /// ([`MCP_WILDCARD_DENY`]) before any answer — and a fresh
    /// `mcp_status` then refines them from the pump.
    fn send_apply_read_only(&self, wanted: PermissionMode) -> Result<(), ProviderError> {
        let request_id = self.next_control_id();
        let on = wanted == PermissionMode::DenyUnmatched;
        let (line, reply) = self.hub.send_read_only_layer(request_id.clone(), wanted);
        self.sent_control.lock().expect("sent control mutex").push(line);
        let delivered = match reply {
            Some(reply) => reply
                .recv()
                .unwrap_or_else(|_| Err(std::io::Error::other("the child's stdin writer has stopped"))),
            None => Ok(()),
        };
        if let Err(error) = delivered {
            self.hub.read_only_layer_unsent(&request_id, on);
            return Err(ProviderError::Unavailable { reason: format!("the session child is unreachable: {error}") });
        }
        if on {
            self.hub.query_mcp_status();
        }
        Ok(())
    }

    /// Record the mode a child was launched in: the chip shows it until the
    /// child's own report (the `initialize` answer, then each `init`)
    /// confirms or corrects it, and only a child launched in Full access
    /// may later switch into it live. A launch with no mode flag announces
    /// nothing — the child's report is the first word.
    fn note_launch_mode(&self, launch: &SessionLaunch) {
        let bypass = launch.permission_mode == Some(PermissionMode::AllowAll);
        *self.bypass_capable.lock().expect("bypass mutex") = bypass;
        if let Some(mode) = launch.permission_mode {
            *self.requested_mode.lock().expect("requested mode mutex") = Some(mode);
        }
        // A fresh child holds only the layer its argv gave it: one applied
        // live died with the child it was applied to.
        let launched = argv::carries_read_only_settings(launch);
        *self.read_only_layer.lock().expect("read-only mutex") =
            if launched { ReadOnlyLayer::Launched } else { ReadOnlyLayer::Off };
        // The `--settings` layer carries no MCP denies: a child that may
        // serve the owner's own servers gets them live at once (see
        // `cover_launched_mcp`), refined by the `mcp_status` answer.
        let owner_mcp = !launch.argv.iter().any(|arg| arg == "--strict-mcp-config");
        self.hub.note_launched_layer(launched, owner_mcp);
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

    /// A child launched Read-only that may serve the owner's own MCP
    /// servers gets the live layer over its `--settings` one at once,
    /// carrying every MCP tool ([`MCP_WILDCARD_DENY`]) until its first
    /// `mcp_status` answer narrows it: nothing it serves is reachable
    /// through an owner allow rule before the denies are known. Best
    /// effort — a refusal banners and owes a relaunch, and a Read-only
    /// turn is held until some MCP layer is confirmed
    /// ([`ControlHub::await_mcp_layer`]).
    fn cover_launched_mcp(&self) {
        let (layer_on, owner_mcp) = {
            let mcp = self.hub.mcp.lock().expect("mcp mutex");
            (mcp.layer_on, mcp.owner_mcp)
        };
        if layer_on && owner_mcp {
            let _ = self.send_apply_read_only(PermissionMode::DenyUnmatched);
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

    /// Bring a lost child back (docs/71 §2): `--resume` the session and
    /// say the connection is restored, so the lane's banner stands down.
    /// `Ok(false)` when nothing was lost.
    fn revive(&self, session_id: &str) -> Result<bool, ProviderError> {
        let Some(launch) = self.revive_launch(session_id) else { return Ok(false) };
        self.relaunch(&launch)?;
        let _ = self.tx.send(ProviderEvent::ConnectionRestored);
        Ok(true)
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
            match crate::home::legacy_owner_home(session_id) {
                Some(owner) => crate::home::owner_config_dir(&owner),
                None => self.owner_config_dir(),
            }
        } else {
            self.resolved_config_dir()
        }
    }

    /// The home override for one spawn: `None` while `session_id`'s
    /// legacy pin stands (the child keeps the owner's real home — the
    /// scrub still applies), else the Baaz home's `CLAUDE_CONFIG_DIR`
    /// pair. Pure over the pin set plus the resolved home, so tests
    /// drive the routing without spawning.
    #[cfg(test)]
    fn home_env_for(&self, session_id: &str) -> Option<Vec<(String, String)>> {
        self.spawn_config_for(session_id).map(|config| crate::home::child_env(&config))
    }

    /// The `CLAUDE_CONFIG_DIR` one spawn for `session_id` carries: `None`
    /// for a legacy pin at the real `HOME` (no override), the pinned owner
    /// home's `.claude` for a pin at another home (an imported session
    /// from a fixture, docs/72), else the Baaz home. The ONE answer both
    /// [`Self::home_env_for`] and the spawns read — the spawns once took
    /// the Baaz home for every override, so an imported session's child
    /// looked for its transcript in the wrong config dir and died "No
    /// conversation found" (docs/72 §6).
    fn spawn_config_for(&self, session_id: &str) -> Option<PathBuf> {
        if crate::home::is_legacy_resume(session_id) {
            crate::home::legacy_owner_home(session_id).map(|owner| crate::home::owner_config_dir(&owner))
        } else {
            Some(self.resolved_config_dir())
        }
    }

    /// Spawn `launch` under its session's home ([`Self::spawn_config_for`]):
    /// a legacy child at the real `HOME` keeps it (no override, the scrub
    /// still applies); every other child names its config dir.
    fn spawn_child(&self, launch: &SessionLaunch) -> std::io::Result<RunningChild> {
        let session_env = self.session_env.lock().expect("session env mutex").clone();
        let with_env;
        let launch = if session_env.is_empty() || !launch.env.is_empty() {
            launch
        } else {
            with_env = SessionLaunch { env: session_env, ..launch.clone() };
            &with_env
        };
        let config = self.spawn_config_for(&launch.session_id);
        self.prepare_home(crate::home::is_legacy_resume(&launch.session_id));
        match config {
            None => RunningChild::spawn_legacy(&self.program, launch, &self.hub),
            Some(config) => RunningChild::spawn(&self.program, &self.with_session_settings(launch.clone()), &self.hub, &config),
        }
    }

    /// What every Baaz-homed child carries beyond its launch, added at
    /// spawn: on `--settings` (merged into the launch's one layer,
    /// [`argv::with_settings`]) `autoMemoryDirectory`, the project memory
    /// the terminal's `claude` shares (docs/85 10.1,
    /// [`crate::home::memory_dir`]; skipped without a known folder);
    /// `idleCompaction` on a claude that has it (docs/85 12.1); the
    /// output-trimming `PostToolUse` hook, Baaz's own binary (12.2,
    /// [`hook`]); and
    /// `--append-system-prompt` with the session's stored note (docs/85
    /// 10.5, [`Self::note_for`]).
    fn with_session_settings(&self, launch: SessionLaunch) -> SessionLaunch {
        let known = self.workspace.lock().expect("workspace mutex").clone();
        let cwd = launch.cwd.as_ref().map(PathBuf::from).or(known);
        let reads_agents_md = self.reads_agents_md();
        let mut extra = serde_json::Map::new();
        if let Some(cwd) = &cwd {
            let redirected = provider::child_env::state_dir_override();
            let memory = crate::home::memory_dir(cwd, &self.owner_config_dir(), redirected.as_deref());
            extra.insert("autoMemoryDirectory".into(), serde_json::Value::String(memory.to_string_lossy().into_owned()));
        }
        if reads_agents_md {
            extra.extend(argv::agents_md_settings());
        }
        // docs/85 12.1: a claude that compacts idle sessions itself does
        // it on the 1 h cache; Baaz runs no timer of its own for it.
        let version = self.version.lock().expect("version mutex").clone();
        if argv::compacts_idle_itself(version.as_deref()) {
            extra.extend(argv::idle_compaction_settings());
        }
        // docs/85 12.2: Baaz's own binary is the PostToolUse hook that trims
        // long Bash output (and, from 2.1.317, stubs a repeated read).
        if let Ok(exe) = std::env::current_exe() {
            extra.extend(hook::hook_settings(&exe, hook::wants_read_stub(version.as_deref())));
        }
        extra.extend(argv::token_settings());
        // docs/85 16.2: the skills the person switched off on the Skills
        // page, as Claude's own `skillOverrides` on the settings layer
        // (rule 26: never in the linked settings.json).
        let off = provider::skill_switches::load(&provider::child_env::state_dir()).off_for("claude-code");
        if !off.is_empty() {
            let overrides: serde_json::Map<String, serde_json::Value> = off.into_iter().map(|name| (name, serde_json::Value::String("off".into()))).collect();
            extra.insert("skillOverrides".into(), serde_json::Value::Object(overrides));
        }
        let mut launch = argv::with_settings(launch, extra);
        // docs/85 16.2: the repo's `.agents/skills`, which claude does not
        // read itself (probed 26 Nov 2026), as links in a folder it is
        // handed with `--add-dir`. A name the home's skills already hold
        // stays theirs.
        if let Some(cwd) = &cwd {
            let taken = self.owner_config_dir().parent().map(crate::home::skill_sources).unwrap_or_default();
            if let Ok(Some(dir)) = crate::home::repo_skills_dir(&provider::child_env::state_dir(), cwd, &taken) {
                launch = argv::with_add_dir(launch, &dir);
            }
        }
        let mut launch = argv::with_token_flags(launch);
        if *self.bills_by_key.lock().expect("bills mutex") {
            launch.env.retain(|(key, _)| key != argv::PROMPT_CACHE_TTL_ENV);
            launch.env.push((argv::PROMPT_CACHE_TTL_ENV.to_owned(), "1h".to_owned()));
        }
        let note = self.note_for(&launch.session_id, cwd.as_deref().filter(|_| !reads_agents_md));
        argv::with_appended_note(launch, &note)
    }

    /// Whether this CLI reads AGENTS.md itself, given the plugin setting
    /// ([`argv::agents_md_settings`]): from 2.1.277 (docs/85 10.2; not
    /// verifiable on 2.1.276, so unit-tested only — "not verified live").
    /// An unknown version reads as no.
    fn reads_agents_md(&self) -> bool {
        self.version
            .lock()
            .expect("version mutex")
            .as_deref()
            .is_some_and(|version| provider::version::compare_versions(version, argv::AGENTS_MD_VERSION) != std::cmp::Ordering::Less)
    }

    /// Baaz's appended note as this build composes it (docs/85 10.5): the
    /// bridge's tools are named only when the bridge rides the session.
    fn compose_note(&self, agents_md_from: Option<&std::path::Path>) -> String {
        let bridge = self.terminal.lock().expect("terminal mutex").is_some();
        let note = provider::note::compose(provider::note::NoteTools { bridge });
        match agents_md_from.and_then(crate::home::agents_md_only) {
            Some(agents) => format!("{note}\n\n{}", argv::agents_md_section(&agents)),
            None => note,
        }
    }

    /// The note `session_id` was opened with — stored beside the Baaz home
    /// (`<state>/notes/`) — else this build's, stored now: every resume,
    /// fork and relaunch of the session sends the same bytes.
    /// `agents_md_from`: the session's folder when this CLI does not read
    /// AGENTS.md itself — a repo with AGENTS.md and no CLAUDE.md then has it
    /// in the note, read once at open (docs/85 10.2).
    fn note_for(&self, session_id: &str, agents_md_from: Option<&std::path::Path>) -> String {
        provider::note::for_session(&self.note_state_dir(), session_id, || self.compose_note(agents_md_from))
    }

    /// Where notes are stored: beside the Baaz home. A unit test that
    /// pinned no home stores them in a temp folder, never the real state
    /// dir (one did, 25 Nov 2026).
    fn note_state_dir(&self) -> PathBuf {
        #[cfg(test)]
        if self.config_override.lock().expect("config mutex").is_none() && self.home_override.is_none() {
            return std::env::temp_dir().join(format!("baaz-test-notes-{}", std::process::id()));
        }
        let home = self.resolved_config_dir();
        home.parent().map(PathBuf::from).unwrap_or(home)
    }

    /// Start the fork [`Command::ForkSession`] pinned for `branch`: the
    /// source resumed with `--fork-session` under the branch's id, cut at
    /// `cut` when the branch names a turn, in the source's own folder. The
    /// branch has no transcript of its own until its first turn, so the
    /// view is shown the source's history through the cut.
    fn spawn_pinned_fork(
        &self,
        branch: &str,
        source: &str,
        cut: Option<&str>,
        mode: Option<PermissionMode>,
        metadata_only: bool,
    ) -> Result<Ack, ProviderError> {
        let path = self.stored_anywhere(source);
        let mut launch = self.launch_for_fork(branch, source)?;
        if let Some(cut) = cut {
            launch = argv::with_resume_at(launch, cut);
        }
        if launch.cwd.is_none() {
            launch.cwd = path.as_deref().and_then(history::transcript_cwd).filter(|cwd| std::path::Path::new(cwd).is_dir());
        }
        let ack = self.spawn_launch(&argv::with_permission_mode(launch, mode))?;
        if metadata_only {
            return Ok(ack);
        }
        let Some(text) = path.and_then(|path| std::fs::read_to_string(path).ok()) else { return Ok(ack) };
        let kept = match cut {
            Some(cut) => {
                let needle = format!("\"uuid\":\"{cut}\"");
                let mut kept = String::new();
                for line in text.lines() {
                    kept.push_str(line);
                    kept.push('\n');
                    if line.replace(' ', "").contains(&needle) {
                        break;
                    }
                }
                kept
            }
            None => text,
        };
        let (deltas, model) = self.fold_stored_text(&kept);
        if let Some(model) = model {
            *self.model.lock().expect("model mutex") = Some(model.clone());
            self.fold.lock().expect("fold mutex").set_model(&model);
        }
        if !deltas.is_empty() {
            let _ = self.tx.send(provider::ProviderEvent::Deltas { session_id: Some(branch.to_owned()), deltas });
        }
        Ok(ack)
    }

    /// A resume launch run in its session's own folder: the cwd the
    /// launch states, else the workspace this adapter knows, else the cwd
    /// the stored transcript records (what P7 import read into the
    /// record's workspace). A folder that no longer exists is dropped —
    /// the child inherits the host's cwd as before (Claude Code still
    /// finds the transcript by id) rather than failing the spawn as a
    /// missing program.
    fn with_resume_cwd(&self, launch: SessionLaunch) -> SessionLaunch {
        if launch.cwd.is_some() {
            return launch;
        }
        let known = self.workspace.lock().expect("workspace mutex").as_ref().map(|path| path.to_string_lossy().into_owned());
        let cwd = known.or_else(|| self.stored_anywhere(&launch.session_id).and_then(|path| history::transcript_cwd(&path)));
        match cwd {
            Some(cwd) if std::path::Path::new(&cwd).is_dir() => SessionLaunch { cwd: Some(cwd), ..launch },
            Some(cwd) => {
                if crate::child::trace_enabled() {
                    eprintln!("baaz: claude resume: folder {cwd} is gone; resuming from the host's cwd");
                }
                launch
            }
            None => launch,
        }
    }

    fn spawn_launch(&self, launch: &SessionLaunch) -> Result<Ack, ProviderError> {
        let (launch, fallback) = self.launch_off_unsupported_auto(launch.clone());
        let launch = &launch;
        let mut child = self.child.lock().expect("child mutex");
        if child.is_some() {
            return Err(ProviderError::Rejected {
                reason: "this adapter already holds a session child; fork or resume instead"
                    .into(),
            });
        }
        // A pre-consent legacy resume spawns under the owner's home (no
        // override at the real `HOME`, scrub kept) so `--resume` finds the
        // transcript where it still lives; everything else spawns
        // Baaz-homed.
        let running = self.spawn_child(launch).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => ProviderError::launch_refusal(
                provider::RefusalHint::NotInstalled,
                "Claude Code isn't installed",
                format!("could not spawn claude: {error}"),
            ),
            _ => ProviderError::Unavailable { reason: format!("could not spawn claude: {error}") },
        })?;
        *child = Some(running);
        // The guard drops here: `send_initialize` locks the child again to
        // write, and holding both would hang the open forever.
        drop(child);
        // Before `initialize` goes out: its answer reports the mode the
        // child really took, and must land after the launched one.
        self.note_launch_mode(launch);
        self.cover_launched_mcp();
        // The catalog arrives on the child's answer to this: without it
        // `ListModels` has nothing to serve but the offline fallback.
        self.send_initialize();
        *self.session_id.lock().expect("session mutex") = Some(launch.session_id.clone());
        // Said once the session is named, so the notice reaches its view.
        if let Some(text) = fallback {
            self.hub.emit_card(CONTROL_MODE_FALLBACK_CARD, text);
        }
        // The launch's `--model`, when one was requested: the fold learns
        // the rest from the `init` frame, but an alias never appears there
        // under its own spelling, so the request is remembered as stated.
        if let Some(model) = &launch.model {
            *self.model.lock().expect("model mutex") = Some(model.clone());
        }
        // The launch's `--effort`, whatever it was — including none: the
        // next `SubmitInput` compares against this; a level from the flag
        // clears back to Default only by relaunching without it.
        *self.effort.lock().expect("effort mutex") = launch.effort.clone();
        *self.launch_effort.lock().expect("launch effort mutex") = launch.effort.clone();
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

    /// Stop the running turn: the `interrupt` control request first, then
    /// — if the turn is still open after [`Self::interrupt_grace`] — end the
    /// child and settle the turn interrupted. Blocks for at most the grace;
    /// commands are dispatched off the UI thread.
    fn stop_turn(&self, session_id: &str) -> Result<Ack, ProviderError> {
        self.check_session(session_id)?;
        if !self.fold.lock().expect("fold mutex").has_live_turn() {
            return Ok(Ack::Accepted);
        }
        let request_id = self.next_control_id();
        self.fold.lock().expect("fold mutex").note_stopping();
        let sent = self.send_control(
            &interrupt_control_request(&request_id),
            PendingControl::bare(PendingControlKind::Interrupt, request_id.clone()),
        );
        if sent.is_ok() {
            let deadline = std::time::Instant::now() + self.interrupt_grace;
            while std::time::Instant::now() < deadline {
                self.hub.drain_confirmations();
                if !self.fold.lock().expect("fold mutex").has_live_turn() {
                    self.hub.untrack(&request_id);
                    return Ok(Ack::Accepted);
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
        self.hub.untrack(&request_id);
        self.end_child_interrupted();
        Ok(Ack::Accepted)
    }

    /// Stop's fallback: hang the child up on purpose (its end of stream is
    /// not a loss), close the open turns as interrupted with no error card,
    /// and leave the session to `--resume` on the next send.
    fn end_child_interrupted(&self) {
        use std::sync::atomic::Ordering;
        self.hub.retire_child(self.hub.child_epoch.load(Ordering::SeqCst));
        if let Some(mut running) = self.child.lock().expect("child mutex").take() {
            running.shutdown();
        }
        self.hub.forget_pending();
        // Not a loss the person must hear about: the next send revives the
        // session with `--resume`, exactly as after a lost child.
        self.hub.child_lost.store(true, Ordering::SeqCst);
        let (session_id, deltas, stopped) = {
            let mut fold = self.fold.lock().expect("fold mutex");
            let session_id = fold.session_id().map(str::to_owned);
            let (deltas, stopped) = fold.interrupt_live_turns();
            (session_id, deltas, stopped)
        };
        if !deltas.is_empty() {
            let _ = self.tx.send(ProviderEvent::Deltas { session_id: session_id.clone(), deltas });
        }
        if let Some(session_id) = session_id {
            for turn_id in stopped {
                let _ = self.tx.send(ProviderEvent::TurnSettled {
                    session_id: session_id.clone(),
                    turn_id,
                    outcome: provider::facts::TurnOutcome::Interrupted,
                });
            }
        }
    }

    /// Tests: shorten Stop's wait for the child to close the turn.
    #[cfg(test)]
    pub(crate) fn set_interrupt_grace(&mut self, grace: std::time::Duration) {
        self.interrupt_grace = grace;
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
            return Err(ProviderError::unknown_approval(approval, "no pending request carries that id"));
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

    /// Answer or decline one `AskUserQuestion` card: settle the card at once,
    /// and send the call's `control_response` when it is complete. Nothing is
    /// claimed twice: the fold removes the call as it builds the line.
    fn settle_ask(
        &self,
        session_id: &str,
        question: &str,
        act: impl FnOnce(&mut ClaudeFold) -> Option<fold::AskStep>,
    ) -> Result<Ack, ProviderError> {
        self.check_session(session_id)?;
        let step = {
            let mut fold = self.fold.lock().expect("fold mutex");
            act(&mut fold).ok_or_else(|| ProviderError::Rejected {
                reason: format!("no question is waiting on {question}"),
            })?
        };
        if let Some(line) = step.line.as_deref() {
            match self.child.lock().expect("child mutex").as_mut() {
                Some(running) => running.send_line(line).map_err(|error| ProviderError::Unavailable {
                    reason: format!("the session child is unreachable: {error}"),
                })?,
                None => return Err(ProviderError::Unavailable { reason: "no session child is running".into() }),
            }
        }
        if !step.deltas.is_empty() {
            let _ = self.tx.send(ProviderEvent::Deltas { session_id: Some(session_id.to_owned()), deltas: step.deltas });
        }
        Ok(Ack::Accepted)
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
            return Err(ProviderError::unknown_approval(approval, "no pending request carries that id"));
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
    ///
    /// The child's report wins; before any report, the mode Baaz last
    /// asked for — never no mode, which would launch a Read-only session
    /// without its deny layer.
    fn with_session_mode(&self, launch: SessionLaunch) -> SessionLaunch {
        match launch.permission_mode {
            Some(_) => launch,
            None => {
                let reported = self.fold.lock().expect("fold mutex").permission_mode();
                let mode = reported.or(*self.requested_mode.lock().expect("requested mode mutex"));
                argv::with_permission_mode(launch, mode)
            }
        }
    }

    /// A child that reports Read-only (`dontAsk`) without the deny layer —
    /// the owner's own `defaultMode` put it there, not Baaz — gets the
    /// layer live before anything else reaches it: `dontAsk` alone still
    /// runs whatever an owner allow rule admits, and the chip must not
    /// read Read-only over that. Runs at each dispatch, so before any turn
    /// goes out; a refused layer banners and owes a relaunch carrying it.
    fn ensure_read_only_layer(&self) {
        let read_only = self.fold.lock().expect("fold mutex").permission_mode() == Some(PermissionMode::DenyUnmatched);
        if !read_only {
            return;
        }
        let mut layer = self.read_only_layer.lock().expect("read-only mutex");
        if *layer != ReadOnlyLayer::Off {
            drop(layer);
            // The owner's MCP servers may have connected (or changed) since
            // the last answer: ask again before the turn goes out.
            self.hub.query_mcp_status();
            return;
        }
        *layer = ReadOnlyLayer::Live;
        drop(layer);
        if self.send_apply_read_only(PermissionMode::DenyUnmatched).is_err() {
            *self.read_only_layer.lock().expect("read-only mutex") = ReadOnlyLayer::Off;
        }
    }

    /// Swap the running child for `launch`: hang up the old one first, then
    /// spawn. The old child's pump ends with its stdout; its folded
    /// transcript stays on the adapter. The relay rides along: an
    /// effort-swap relaunch is still the same session, so the resumed child
    /// sees the same bridge the old one did.
    /// What [`Self::relaunch`] spawns for `launch`: the bridge config and
    /// the session's mode applied, Auto left for Ask on a model without it
    /// (with the notice to show), and a `--resume` of a session the CLI
    /// never stored — no turn reached it yet — opened fresh under its own
    /// id instead ([`argv::as_fresh_open`]: `--resume` exits "No
    /// conversation found"). Both run in the session's workspace. Pure
    /// apart from the bridge config file — no spawn.
    fn relaunch_launch(&self, launch: &SessionLaunch) -> Result<(SessionLaunch, Option<String>), ProviderError> {
        let launch = self.with_session_mode(self.with_mcp_config(launch.clone())?);
        let (launch, fallback) = self.launch_off_unsupported_auto(launch);
        let workspace =
            self.workspace.lock().expect("workspace mutex").as_ref().map(|path| path.to_string_lossy().into_owned());
        let launch = if self.find_stored(&launch.session_id).is_none() {
            argv::as_fresh_open(launch, workspace.as_deref())
        } else {
            SessionLaunch { cwd: launch.cwd.clone().or(workspace), ..launch }
        };
        Ok((launch, fallback))
    }

    fn relaunch(&self, launch: &SessionLaunch) -> Result<(), ProviderError> {
        if let Some(running) = self.child.lock().expect("child mutex").as_mut() {
            running.shutdown();
        }
        *self.child.lock().expect("child mutex") = None;
        // Requests the old child never answered die with it (Y1b review).
        self.hub.forget_pending();
        // So do the lines it never answered: its end of stream is
        // epoch-gated (a replaced child's never reaches the fold), so
        // nothing else clears them, and a stale one would pair the new
        // child's next turn with an old ack. And so do its asks: an answer
        // under its request id must never reach the new child (PM-14).
        let (session_id, dropped) = {
            let mut fold = self.fold.lock().expect("fold mutex");
            fold.forget_submissions();
            (fold.session_id().map(str::to_owned), fold.drop_pending_asks())
        };
        if !dropped.is_empty() {
            let _ = self.tx.send(ProviderEvent::Deltas { session_id, deltas: dropped });
        }
        let (launch, fallback) = self.relaunch_launch(launch)?;
        // An effort-swap relaunch is still the same session: a legacy
        // resume keeps the owner's home here too.
        let running = self.spawn_child(&launch).map_err(|error| {
            ProviderError::Unavailable { reason: format!("could not respawn claude: {error}") }
        })?;
        *self.child.lock().expect("child mutex") = Some(running);
        // A relaunched child restates the whole session, catalog included
        // — and its mode, which must land after the launched one.
        self.note_launch_mode(&launch);
        self.cover_launched_mcp();
        self.send_initialize();
        *self.session_id.lock().expect("session mutex") = Some(launch.session_id.clone());
        // Said once the session is named, so the notice reaches its view.
        if let Some(text) = fallback {
            self.hub.emit_card(CONTROL_MODE_FALLBACK_CARD, text);
        }
        if let Some(model) = &launch.model {
            *self.model.lock().expect("model mutex") = Some(model.clone());
        }
        *self.effort.lock().expect("effort mutex") = launch.effort.clone();
        *self.launch_effort.lock().expect("launch effort mutex") = launch.effort.clone();
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
                provider::SubmissionPart::Image { base64_data, media_type, .. } => {
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
        // The line's own uuid, which the CLI's echo repeats: the turn that
        // opens after that echo is this submission's.
        let uuid = argv::line_uuid();
        let line = argv::user_content_line(text, images, Some(&uuid));
        // The bubble goes up now, not on the echo: a child that dies
        // before echoing still leaves what was sent on screen. Noted
        // before the line goes out: the pump may fold the turn's first
        // message before this thread locks the fold again.
        let bubble = {
            let mut fold = self.fold.lock().expect("fold mutex");
            let shown: Vec<frame::UserImage> = images
                .iter()
                .map(|image| frame::UserImage { media_type: image.media_type.clone(), data_len: image.base64_data.len() })
                .collect();
            let bubble = fold.submitted_user_turn(&uuid, text, &shown);
            fold.note_submission_line(&turn_id, &uuid);
            bubble
        };
        if !bubble.is_empty() {
            let _ = self.tx.send(ProviderEvent::Deltas { session_id: Some(session_id.to_owned()), deltas: bubble });
        }
        let sent = match self.child.lock().expect("child mutex").as_mut() {
            Some(running) => running.send_line(&line).map_err(|error| ProviderError::Unavailable {
                reason: format!("the session child is unreachable: {error}"),
            }),
            None => Err(ProviderError::Unavailable { reason: "no session child is running".into() }),
        };
        if let Err(error) = sent {
            let retracted = {
                let mut fold = self.fold.lock().expect("fold mutex");
                fold.forget_submission(&turn_id);
                fold.retract_submitted_user_turn(&uuid)
            };
            if !retracted.is_empty() {
                let _ = self.tx.send(ProviderEvent::Deltas { session_id: Some(session_id.to_owned()), deltas: retracted });
            }
            return Err(error);
        }
        // The ack carries the submission handle: the CLI names the turn
        // only when its first message streams, and the fold announces
        // that id as `TurnIdentified` against this one.
        Ok(Ack::TurnAccepted { turn_id, queued: false })
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

    /// Whether any `<id>.jsonl` exists for `session_id` under the session's
    /// config dir: the session's own slug directory, or any other (two
    /// matches still count — the CLI may resolve them; only none at all
    /// means the history is gone).
    fn transcript_exists_anywhere(&self, session_id: &str) -> bool {
        self.stored_anywhere(session_id).is_some()
    }

    /// The session's own stored transcript ([`Self::find_stored`]), else
    /// the first `<id>.jsonl` under any slug directory of its config dir.
    fn stored_anywhere(&self, session_id: &str) -> Option<PathBuf> {
        if let Some(found) = self.find_stored(session_id) {
            return Some(found);
        }
        if session_id.is_empty() || session_id.contains('/') {
            return None;
        }
        let projects = self.config_dir_for(session_id).join("projects");
        let entries = std::fs::read_dir(&projects).ok()?;
        let file_name = format!("{session_id}.jsonl");
        entries.filter_map(|entry| entry.ok()).map(|entry| entry.path().join(&file_name)).find(|path| path.is_file())
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
        Ok(self.fold_stored_text(&text))
    }

    /// Fold stored transcript `text` the way a resume shows it: the
    /// history's deltas and the model it last ran on.
    fn fold_stored_text(&self, text: &str) -> (Vec<Delta>, Option<String>) {
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
        (deltas, fold.stored_model().map(str::to_owned))
    }
}

/// The notice that `model` left Auto for Ask: its catalog name when a row
/// names it, else "Haiku" for the family [`argv::model_lacks_auto`] knows
/// without a row, else the id as given.
fn auto_fallback_text(model: &str, row: Option<&frame::CatalogModel>) -> String {
    let name = match row {
        Some(row) => row.display_name.clone(),
        None if frame::normalize_model_id(model).starts_with("haiku") => "Haiku".to_owned(),
        None => model.to_owned(),
    };
    format!("Auto is not available for {name}; Claude Code will ask before acting.")
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
            return Err(ProviderError::launch_refusal(
                provider::RefusalHint::TooOld,
                "Claude Code needs an update",
                format!("claude {version} is below the floor {CLAUDE_VERSION_FLOOR}; refusing"),
            ));
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
        // A model switch the child confirmed since: Auto follows the model
        // it actually runs, before this command reads the mode. A mode pick
        // is the person's answer already — it supersedes the fallback, so
        // no stale banner lands beside the mode they just chose.
        if let Some(model) = self.hub.take_confirmed_model() {
            if !matches!(command, Command::SelectApprovalMode { .. }) {
                self.leave_auto_if_unsupported(&model);
            }
        }
        self.ensure_read_only_layer();
        match command {
            Command::OpenSession { request_id, workspace, model, mode, env, .. } => {
                *self.session_env.lock().expect("session env mutex") = env;
                let launch =
                    self.launch_for_open(&request_id, workspace.as_deref(), model.as_deref())?;
                self.spawn_launch(&argv::with_permission_mode(launch, mode))
            }
            Command::ResumeSession { session_id, mode, env, metadata_only, .. } => {
                // The resumed lane's children carry the session's variables
                // as an opened one's do (docs/82 6.2).
                *self.session_env.lock().expect("session env mutex") = env;
                // No transcript anywhere under the config dir: `--resume`
                // can only die on it ("No conversation found"), which read
                // as a lost child and promised to "pick up where it left
                // off" over an empty view (QA 2026-10-30). Refused typed
                // instead, before any spawn.
                if let Some((source, cut)) = crate::home::take_fork(&session_id) {
                    return self.spawn_pinned_fork(&session_id, &source, cut.as_deref(), mode, metadata_only);
                }
                if !self.transcript_exists_anywhere(&session_id) {
                    return Err(ProviderError::launch_refusal(
                        provider::RefusalHint::HistoryMissing,
                        "Session history missing",
                        format!("no stored transcript for session {session_id} on this Mac"),
                    ));
                }
                let launch = argv::with_permission_mode(self.launch_for_resume(&session_id)?, mode);
                let launch = self.with_resume_cwd(launch);
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
                // `metadata_only`: the host shows the transcript from its
                // own store (docs/85 11.1) — nothing is folded or sent.
                if metadata_only {
                    return Ok(ack);
                }
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
            Command::ForkSession { request_id, session_id, through_turn, .. } => {
                // One child per adapter: the source's adapter already runs
                // the source, so the branch starts in the NEW session's
                // adapter, which the host opens with `ResumeSession` on the
                // id acked here (found live 26 Nov 2026: forking in place
                // was refused as "already holds a session child"). Here the
                // source is checked and the fork pinned for that resume.
                if !self.transcript_exists_anywhere(&session_id) {
                    return Err(ProviderError::Unavailable {
                        reason: format!("no stored transcript for session {session_id}, so there is nothing to fork"),
                    });
                }
                // docs/85 16.3: a branch through a turn is cut at that
                // turn's last transcript row. A turn the transcript does
                // not hold is refused, never forked whole.
                let cut = match through_turn {
                    Some(turn) => {
                        let transcript = self.stored_anywhere(&session_id).and_then(|path| std::fs::read_to_string(path).ok());
                        let Some(cut) = transcript.as_deref().and_then(|text| history::cut_uuid_for_turn(text, &turn)) else {
                            return Err(ProviderError::Unavailable {
                                reason: format!("the turn {turn} is not in session {session_id}'s transcript, so there is nothing to branch from"),
                            });
                        };
                        Some(cut)
                    }
                    None => None,
                };
                crate::home::pin_fork(&request_id, &session_id, cut.as_deref());
                Ok(Ack::Session { session_id: request_id, title: None })
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
                        .map(|session_id| SessionSummary { session_id, ..Default::default() })
                        .collect(),
                    next_cursor: None,
                })
            }
            Command::ReadSession { session_id, .. } => match self.find_stored(&session_id) {
                Some(_) => Ok(Ack::SessionRead { session_id, title: None, first_shell_command: None }),
                None => Err(ProviderError::Unavailable {
                    reason: format!(
                        "no stored transcript for session {session_id}: refusing to guess a \
                         different file"
                    ),
                }),
            },
            // `/compact` sent as a user message compacts the session and
            // answers with a `compact_boundary` (probed live 24 and 25 Nov
            // 2026, docs/85-evidence/10.4). It runs as a turn of its own —
            // never the person's: its echo does not bubble, and the app
            // knows the turn by its lone "Context compacted" row (no
            // title, byline, count or unread from it). A turn in flight
            // is refused: the line would queue behind it as a prompt.
            Command::CompactSession { session_id, focus, .. } => {
                self.check_session(&session_id)?;
                self.revive(&session_id)?;
                if self.fold.lock().expect("fold mutex").turn_open() {
                    return Err(ProviderError::Rejected { reason: "a turn is running; compact when it finishes".into() });
                }
                let uuid = argv::line_uuid();
                let line = argv::user_content_line(&argv::compact_text(focus.as_deref()), &[], Some(&uuid));
                let opened = self.fold.lock().expect("fold mutex").begin_compaction(&uuid);
                if !opened.is_empty() {
                    let _ = self.tx.send(ProviderEvent::Deltas { session_id: Some(session_id.clone()), deltas: opened });
                }
                match self.child.lock().expect("child mutex").as_mut() {
                    Some(running) => running.send_line(&line).map_err(|error| ProviderError::Unavailable {
                        reason: format!("the session child is unreachable: {error}"),
                    })?,
                    None => return Err(ProviderError::Unavailable { reason: "no session child is running".into() }),
                }
                Ok(Ack::Accepted)
            }
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
                // Auto is re-checked when the child confirms the switch
                // (see `leave_auto_if_unsupported`), not here.
                Ok(Ack::Accepted)
            }
            Command::SelectApprovalMode { session_id, mode, .. } => {
                self.check_session(&session_id)?;
                *self.requested_mode.lock().expect("requested mode mutex") = Some(mode);
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
            // Its own `plan` permission mode, live over the control
            // channel (docs/82 5.5). Off returns to the mode the chip
            // reads: plan folds to no neutral mode, so the fold still holds
            // the mode the session was in. The child's `status` frame then
            // reports the move, and the pump says so with
            // `PlanModeChanged`.
            // Claude Code takes its goal as its own `/goal` message
            // (`ProviderTrait::GoalSlashCommand`): the app sends the words.
            Command::SetGoal { .. } => Err(ProviderError::unsupported(
                "set-goal",
                "Claude Code takes a goal as its own `/goal` message, sent as a turn",
            )),
            Command::SetPlanMode { session_id, on, .. } => {
                self.check_session(&session_id)?;
                let back = self.fold.lock().expect("fold mutex").permission_mode().unwrap_or_default();
                self.send_set_plan_mode(on, back)?;
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
                // Default rides it too (`effortLevel: null`, probed live
                // 25 Nov 2026), except for a level the child was launched
                // with (`--effort`), which keeps the `--resume` relaunch
                // path (the relaunched child spawns without it). A rejected
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
                self.revive(&session_id)?;
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
                        None if self.launch_effort.lock().expect("launch effort mutex").is_none() => {
                            self.check_session(&session_id)?;
                            self.send_reset_effort(recorded)?;
                            *self.effort.lock().expect("effort mutex") = None;
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
                // Read-only with the owner's own MCP servers: the turn waits
                // for the child to confirm the layer's MCP denies (this
                // dispatch's `mcp_status` answer and the apply carrying
                // it), failing closed (see `ControlHub::await_mcp_layer`).
                // The CLI emits no frame when a server's tool list changes
                // mid-turn (2.1.276 has none on stream-json), so a tool a
                // server adds during a turn is covered from the next one.
                self.hub
                    .await_mcp_layer(MCP_LAYER_HOLD)
                    .map_err(|reason| ProviderError::Unavailable { reason })?;
                let (text, images) = Self::split_parts(&parts);
                self.submit_parts(&session_id, &text, &images, request_id, display_text.as_deref())
            }
            // Unverified, so attempted: a second stdin frame mid-turn is the
            // only lane the process shape offers, unprobed as it is.
            Command::SteerInput { request_id, session_id, parts, .. } => {
                let _ = request_id;
                let (text, images) = Self::split_parts(&parts);
                self.check_session(&session_id)?;
                // The CLI answers a mid-turn line with a turn and a
                // `result` of its own (probed live 2026-10-16): noted as a
                // steer under its own uuid, so that turn and that result
                // close the steer and never a queued submission.
                let uuid = argv::line_uuid();
                let line = argv::user_content_line(&text, &images, Some(&uuid));
                self.fold.lock().expect("fold mutex").note_steer(&uuid);
                let sent = match self.child.lock().expect("child mutex").as_mut() {
                    Some(running) => running.send_line(&line).map_err(|error| ProviderError::Unavailable {
                        reason: format!("the session child is unreachable: {error}"),
                    }),
                    None => Err(ProviderError::Unavailable { reason: "no session child is running".into() }),
                };
                if let Err(error) = sent {
                    self.fold.lock().expect("fold mutex").forget_line(&uuid);
                    return Err(error);
                }
                Ok(Ack::Accepted)
            }
            // TurnControl is Emulated (docs/82 0.8): ask the child to
            // interrupt, and if the turn has not closed within the grace,
            // end the child and mark the turn interrupted — Stop always
            // stops. Cancel is the same act on this CLI.
            Command::InterruptTurn { session_id, .. } | Command::CancelTurn { session_id, .. } => {
                self.stop_turn(&session_id)
            }
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
                            default_effort: None,
                            effort_notes: Vec::new(),
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
                    questions: fold
                        .pending_asks()
                        .into_iter()
                        .map(|(id, headline)| provider::PendingQuestion { id, session_id: session_id.clone(), headline })
                        .collect(),
                })
            }
            Command::AnswerQuestion { session_id, question, answers, .. } => {
                let answer = answers.into_iter().next().ok_or_else(|| ProviderError::Rejected {
                    reason: format!("no answer given for {question}"),
                })?;
                let reply = match (answer.selected_labels, answer.selected_label, answer.free_text) {
                    (Some(labels), _, _) if !labels.is_empty() => fold::AskReply::Picked(labels),
                    (_, Some(label), _) => fold::AskReply::Picked(vec![label]),
                    (_, _, Some(text)) if !text.trim().is_empty() => fold::AskReply::Text(text),
                    _ => return Err(ProviderError::Rejected { reason: format!("an empty answer for {question}") }),
                };
                self.settle_ask(&session_id, &question, |fold| fold.answer_ask(&question, reply))
            }
            Command::DismissQuestion { session_id, question, reason, .. } => {
                let message = reason.unwrap_or_else(|| String::from("The user chose not to answer."));
                self.settle_ask(&session_id, &question, |fold| fold.dismiss_ask(&question, &message))
            }
            Command::ClarifyQuestion { session_id, question, text, .. } => {
                self.settle_ask(&session_id, &question, |fold| fold.answer_ask(&question, fold::AskReply::Text(text)))
            }
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
            // "Retry now" on a lost child: resume it here, without a
            // turn. A live child needs nothing.
            Command::FollowSession { session_id, .. } => {
                if !self.revive(&session_id)? {
                    self.require_child()?;
                }
                Ok(Ack::Accepted)
            }
            Command::UnfollowSession { .. } => Ok(Ack::Accepted),
            Command::ReadStoredOutput { .. } => Err(ProviderError::unsupported(
                "read-stored-output",
                "the CLI exposes no stored-output reference; page the stored transcript instead",
            )),
            // The reading the fold holds, the same peek `read_usage` makes.
            Command::ReadUsage => Ok(Ack::Usage { report: self.read_usage() }),
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
                Ok(Ack::Account {
                    signed_in: label.is_some(),
                    lane: if label.is_some() { provider::AccountLane::AccountLogin } else { provider::AccountLane::LoggedOut },
                    label,
                })
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

    fn stderr_tail(&self) -> Vec<String> {
        self.hub.stderr.lines()
    }

    fn shutdown(&mut self) {
        // Poison-tolerant: the child is hung up even when a panic left the
        // lock poisoned (docs/81 B PM-3).
        let mut child = self.child.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(running) = child.as_mut() {
            running.shutdown();
        }
        *child = None;
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

    /// The program an adapter that must never spawn is built with: a path
    /// that cannot exist, so a regression that starts a child fails to
    /// spawn instead of reaching the installed CLI (the no-real-CLI
    /// ratchet, `provider/tests/no_real_cli.rs`).
    const NO_CLI: &str = "/nonexistent/baaz-test/claude";

    fn meter_line() -> &'static str {
        r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","resetsAt":1790384400,"rateLimitType":"seven_day","utilization":0.97,"isUsingOverage":true,"surpassedThreshold":0.75,"unifiedWindows":{"five_hour":{"utilization":0.9,"resetsAt":1790187000},"seven_day":{"utilization":0.97,"resetsAt":1790384400}}}}"#
    }

    #[test]
    fn read_account_is_false_until_a_meter_reading_is_seen() {
        // Fresh adapter: no meter observed, so no login claimed. A
        // hardcoded `true` fails this test.
        let adapter = ClaudeCodeAdapter::new(NO_CLI);
        let ack = adapter.dispatch(Command::ReadAccount).expect("account reads");
        assert!(
            matches!(ack, Ack::Account { signed_in: false, label: None, .. }),
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

    /// Owner 2026-10-21: the effort menu offered only Default because the
    /// view's attach-time `ListModels` came before this answer and nothing
    /// asked again. The answer now tells the view its catalog is here.
    #[test]
    fn the_initialize_answer_announces_the_catalog() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("s-1");
        adapter.hub.track(PendingControl {
            kind: PendingControlKind::Initialize,
            request_id: "req_init_1".into(),
            wanted_model: None,
            previous_model: None,
            previous_fold_model: None,
            wanted_effort: None,
            previous_effort: None,
            wanted_mcp: None,
            sent_at: Instant::now(),
        });
        let events = adapter.events();
        let path = format!("{}/../../fixtures/claude-code/permission.jsonl", env!("CARGO_MANIFEST_DIR"));
        for line in std::fs::read_to_string(path).expect("fixture reads").lines().filter(|l| !l.trim().is_empty()) {
            adapter.ingest_child_line(line);
        }
        let mut announced = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let ProviderEvent::ModelCatalogChanged { session_id } = event {
                announced.push(session_id);
            }
        }
        assert_eq!(announced, ["s-1"], "one announcement, for the session the child serves");
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
                wanted_mcp: None,
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

    /// docs/85 10.3: a level raised live clears back to Default over the
    /// control channel — `apply_flag_settings{effortLevel: null}` — with no
    /// relaunch (no spawn: the stand-in program must never run); a level
    /// the child was launched with (`--effort`) still relaunches flagless.
    #[test]
    fn clearing_a_live_raised_effort_sends_null_and_never_relaunches() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-9");
        *adapter.effort.lock().expect("effort mutex") = Some("high".into());
        let _ = adapter.dispatch(Command::SubmitInput {
            request_id: "t-1".into(),
            session_id: "sess-9".into(),
            parts: vec![provider::SubmissionPart::Text("hi".into())],
            display_text: None,
            effort: None,
        });
        let lines: Vec<serde_json::Value> =
            adapter.sent_control_lines().iter().map(|line| serde_json::from_str(line).expect("json")).collect();
        let reset = lines
            .iter()
            .find(|line| line["request"]["subtype"] == "apply_flag_settings")
            .unwrap_or_else(|| panic!("a reset line: {lines:?}"));
        assert_eq!(reset["request"]["settings"], serde_json::json!({"effortLevel": null}));
        assert_eq!(adapter.effort.lock().expect("effort mutex").clone(), None, "Default from the next turn");
        assert!(adapter.hub.take_effort_relaunch().is_none(), "no relaunch owed");
        // A refused reset owes a flagless relaunch, bannered as Default.
        adapter.ingest_child_line(&error_line(&pending_id(&adapter), "nope"));
        let launch = adapter.hub.take_effort_relaunch().expect("a refusal owes a relaunch");
        assert!(!launch.argv.iter().any(|arg| arg == "--effort"), "{:?}", launch.argv);
        let cards = control_cards(&drain_events(&adapter));
        assert!(cards.iter().any(|card| card.1.contains("\"Default\"")), "{cards:?}");
        // From the launch flag: only a relaunch clears it.
        *adapter.launch_effort.lock().expect("launch effort mutex") = Some("high".into());
        *adapter.effort.lock().expect("effort mutex") = Some("high".into());
        assert!(adapter.resume_launch_for_effort("sess-9", None).is_some());
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
        // Pinned at another owner home (an imported session from a fixture,
        // docs/72): the child and the history read that home's `.claude`.
        let fixture = PathBuf::from("/tmp/baaz-import-fixture-owner");
        crate::home::pin_legacy_resume_at(&id, &fixture);
        let env = adapter.home_env_for(&id).expect("a fixture home is named");
        assert!(
            env.iter().any(|(name, value)| name == "CLAUDE_CONFIG_DIR"
                && value == "/tmp/baaz-import-fixture-owner/.claude"),
            "the child resumes from the fixture's config dir: {env:?}"
        );
        assert_eq!(adapter.config_dir_for(&id), fixture.join(".claude"), "history reads it too");
        crate::home::clear_legacy_resume(&id);
        assert!(adapter.home_env_for(&id).is_some(), "a cleared pin resumes Baaz-homed");
    }

    /// docs/85 16.3 (probed live 26 Nov 2026): Branch through a turn on a
    /// running session. The source's adapter holds its own child, so it
    /// only checks the cut and acks the branch id (forking in place was
    /// refused live as "already holds a session child"); the branch's own
    /// adapter, opened with `ResumeSession` on that id, spawns the fork with
    /// `--resume-session-at` at the turn's last row and shows the history
    /// through it. A turn the transcript does not hold is refused, never
    /// forked whole. Against a stand-in `claude` that records its argv.
    #[test]
    fn a_branch_through_a_turn_cuts_the_fork_at_that_turns_last_row() {
        let config = std::env::temp_dir().join(format!("baaz-claude-fork-cut-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&config);
        let slug_dir = config.join("projects").join("-fixture-proj");
        std::fs::create_dir_all(&slug_dir).expect("slug dir");
        let source = "139c8757-db8e-4243-a440-de494311ee11";
        std::fs::write(slug_dir.join(format!("{source}.jsonl")), include_str!("../../../fixtures/claude-code/fork-source.ndjson")).expect("transcript");
        let (dir, _) = seed_executable("fork-cut", "unused", "");
        let seen = dir.join("argv");
        let script = dir.join("fake-claude");
        std::fs::write(&script, format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{0}.tmp'; mv '{0}.tmp' '{0}'\nexec cat >/dev/null\n", seen.display())).expect("fake");
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let fork = |adapter: &ClaudeCodeAdapter, branch: &str, turn: &str| {
            adapter.dispatch(Command::ForkSession {
                request_id: branch.into(),
                session_id: source.into(),
                through_turn: Some(turn.into()),
                metadata_only: false,
                mode: None,
            })
        };
        // The source's adapter: no spawn, the branch id acked.
        let source_adapter = ClaudeCodeAdapter::new("claude-must-never-spawn").with_config_dir(config.clone());
        let ack = fork(&source_adapter, "branch-1", "msg_011ChQyGv7gFNYb56LXj8TTf");
        assert!(matches!(&ack, Ok(Ack::Session { session_id, .. }) if session_id == "branch-1"), "{ack:?}");

        // The branch's own adapter starts the fork, cut at the turn.
        let branch_adapter = ClaudeCodeAdapter::new(&script.to_string_lossy()).with_config_dir(config.clone());
        let events = branch_adapter.events();
        let ack = branch_adapter.dispatch(Command::ResumeSession {
            request_id: "r".into(),
            session_id: "branch-1".into(),
            cursor: None,
            metadata_only: false,
            mode: None,
            env: Vec::new(),
            workspace: None,
        });
        assert!(matches!(ack, Ok(Ack::Session { .. })), "the branch started: {ack:?}");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !seen.is_file() {
            assert!(std::time::Instant::now() < deadline, "the stand-in never started");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let shown: String = events.try_iter().filter_map(|event| match event {
            provider::ProviderEvent::Deltas { deltas, .. } => Some(format!("{deltas:?}")),
            _ => None,
        }).collect();
        drop(branch_adapter);
        let argv: Vec<String> = std::fs::read_to_string(&seen).expect("argv").lines().map(str::to_owned).collect();
        let after = |flag: &str| argv.iter().position(|arg| arg == flag).map(|at| argv[at + 1].clone());
        assert_eq!(after("--resume").as_deref(), Some(source));
        assert_eq!(after("--resume-session-at").as_deref(), Some("68477577-b2e6-478d-8bf3-d287ec563f2d"));
        assert_eq!(after("--session-id").as_deref(), Some("branch-1"));
        assert!(argv.iter().any(|arg| arg == "--fork-session"));
        assert!(shown.contains("ALPHA") && !shown.contains("BETA"), "the branch shows the history through the cut only");

        let error = fork(&source_adapter, "branch-2", "msg_not_in_it").expect_err("an unknown turn is refused");
        assert!(format!("{error:?}").contains("nothing to branch from"), "{error:?}");
        assert!(crate::home::take_fork("branch-2").is_none(), "nothing pinned for a refused branch");
        let _ = std::fs::remove_dir_all(&config);
    }

    /// A stand-in `claude` that records where and under which config dir
    /// it was started, then idles on stdin until hung up. Never a real CLI.
    fn recording_claude(tag: &str) -> (PathBuf, PathBuf) {
        let (dir, _) = seed_executable(tag, "unused", "");
        let out = dir.join("seen");
        std::fs::create_dir_all(&out).expect("record dir");
        let body = format!(
            "#!/bin/sh\npwd -P > '{out}/cwd.tmp'; mv '{out}/cwd.tmp' '{out}/cwd'\n\
             printf '%s' \"$CLAUDE_CONFIG_DIR\" > '{out}/config.tmp'; mv '{out}/config.tmp' '{out}/config'\n\
             exec cat >/dev/null\n",
            out = out.display()
        );
        let script = dir.join("fake-claude");
        std::fs::write(&script, body).expect("fake writes");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        (script, out)
    }

    /// What [`recording_claude`] saw: its cwd and its `CLAUDE_CONFIG_DIR`.
    fn seen_by(out: &std::path::Path) -> (String, String) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !(out.join("cwd").is_file() && out.join("config").is_file()) {
            assert!(std::time::Instant::now() < deadline, "the stand-in never started");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let read = |name: &str| std::fs::read_to_string(out.join(name)).expect("recorded").trim().to_owned();
        (read("cwd"), read("config"))
    }

    /// docs/82 6.1: `OpenSession.env` reaches the session's child — a
    /// worktree session's port block is what the agent's own commands
    /// see. Against a stand-in `claude` that prints its environment.
    #[test]
    fn an_open_starts_the_child_with_the_sessions_env() {
        let root = scratch_dir("open-env");
        let (dir, _) = seed_executable("open-env", "unused", "");
        let out = dir.join("env.txt");
        let script = dir.join("fake-claude");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s|%s|%s|%s' \"$BAAZ_WORKTREE_PATH\" \"$BAAZ_PROJECT_ROOT\" \"$BAAZ_PORT\" \"$BAAZ_PORT_COUNT\" > '{out}.tmp'; mv '{out}.tmp' '{out}'\nexec cat >/dev/null\n",
                out = out.display()
            ),
        )
        .expect("fake writes");
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let adapter = ClaudeCodeAdapter::new(&script.to_string_lossy()).with_config_dir(root.join("home"));
        let env = vec![
            ("BAAZ_WORKTREE_PATH".to_owned(), "/w/s-1".to_owned()),
            ("BAAZ_PROJECT_ROOT".to_owned(), "/p".to_owned()),
            ("BAAZ_PORT".to_owned(), "41010".to_owned()),
            ("BAAZ_PORT_COUNT".to_owned(), "10".to_owned()),
        ];
        let ack = adapter.dispatch(Command::OpenSession {
            request_id: format!("env-{}", std::process::id()),
            workspace: Some(root.to_string_lossy().into_owned()),
            model: None,
            model_provider: None,
            mode: None,
            env,
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !out.is_file() {
            assert!(std::time::Instant::now() < deadline, "the stand-in never started");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        drop(adapter);
        assert!(matches!(ack, Ok(Ack::Session { .. })), "opened: {ack:?}");
        assert_eq!(std::fs::read_to_string(&out).expect("recorded"), "/w/s-1|/p|41010|10");
    }

    /// docs/85 10.1: a spawned child carries the shared project memory on
    /// its one `--settings` layer — the owner's `~/.claude/projects/<slug
    /// of the main repo>/memory` (here a stand-in owner home), merged with
    /// the Read-only layer when the session is Read-only. Against a
    /// stand-in `claude` that records its argv. Never a real CLI.
    #[test]
    fn a_spawned_child_carries_the_shared_memory_folder() {
        let root = scratch_dir("memory-argv");
        let owner = root.join("owner");
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).expect("repo");
        std::fs::create_dir_all(repo.join("src")).expect("subfolder");
        let (dir, _) = seed_executable("memory-argv", "unused", "");
        let out = dir.join("argv.txt");
        let script = dir.join("fake-claude");
        std::fs::write(
            &script,
            format!("#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{out}.tmp'; mv '{out}.tmp' '{out}'\nexec cat >/dev/null\n", out = out.display()),
        )
        .expect("fake writes");
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        for mode in [None, Some(PermissionMode::DenyUnmatched)] {
            let _ = std::fs::remove_file(&out);
            let adapter = ClaudeCodeAdapter::new(&script.to_string_lossy()).with_home(owner.clone()).with_config_dir(root.join("home"));
            let ack = adapter.dispatch(Command::OpenSession {
                request_id: format!("mem-{}-{}", std::process::id(), mode.is_some()),
                workspace: Some(repo.join("src").to_string_lossy().into_owned()),
                model: None,
                model_provider: None,
                mode,
                env: Vec::new(),
            });
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !out.is_file() {
                assert!(std::time::Instant::now() < deadline, "the stand-in never started");
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            drop(adapter);
            assert!(matches!(ack, Ok(Ack::Session { .. })), "opened: {ack:?}");
            let argv: Vec<String> = std::fs::read_to_string(&out).expect("recorded").lines().map(str::to_owned).collect();
            assert_eq!(argv.iter().filter(|arg| *arg == "--settings").count(), 1, "one layer: {argv:?}");
            let at = argv.iter().position(|arg| arg == "--settings").expect("--settings");
            let settings: serde_json::Value = serde_json::from_str(&argv[at + 1]).expect("settings parse");
            let expected = crate::home::memory_dir(&repo, &owner.join(".claude"), provider::child_env::state_dir_override().as_deref());
            assert_eq!(settings["autoMemoryDirectory"], serde_json::json!(expected.to_string_lossy()), "{settings}");
            assert!(!expected.to_string_lossy().contains("-src"), "the main repo's slug, not the subfolder's: {}", expected.display());
            assert_eq!(settings.get("disableSkillShellExecution").is_some(), mode.is_some(), "Read-only rides the same layer: {settings}");
            // docs/85 12.2: the output hook is this binary, on Bash, in the
            // same one layer.
            let hook = &settings["hooks"]["PostToolUse"][0];
            assert_eq!(hook["matcher"], "Bash", "{settings}");
            let command = hook["hooks"][0]["command"].as_str().expect("hook command");
            assert!(command.ends_with(" --tool-output-hook"), "{command}");
            assert!(command.contains(&*std::env::current_exe().unwrap().to_string_lossy()), "the running binary: {command}");
        }
    }

    /// docs/85 10.5: every spawn of a session carries Baaz's note on one
    /// `--append-system-prompt` (never `--system-prompt`), and it is the
    /// note the session was opened with: the stored bytes win over this
    /// build's wording, so a relaunch keeps the prompt prefix.
    #[test]
    fn every_spawn_appends_the_sessions_stored_note() {
        let root = scratch_dir("note-argv");
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn").with_config_dir(root.join("home"));
        let launch = || argv::argv_for_resume("sess-note", None, None, None);
        let flag = |launch: &SessionLaunch| {
            assert_eq!(launch.argv.iter().filter(|arg| *arg == "--append-system-prompt").count(), 1, "{:?}", launch.argv);
            assert!(!launch.argv.iter().any(|arg| arg == "--system-prompt"), "never replaces the vendor's prompt");
            let at = launch.argv.iter().position(|arg| arg == "--append-system-prompt").expect("flag");
            launch.argv[at + 1].clone()
        };
        let first = flag(&adapter.with_session_settings(launch()));
        assert_eq!(first, provider::note::compose(provider::note::NoteTools { bridge: false }));
        assert_eq!(flag(&adapter.with_session_settings(launch())), first, "byte-identical on the next spawn");
        let stored = provider::note::stored_path(&root, "sess-note").expect("a path");
        std::fs::write(&stored, "the wording it opened with").expect("write");
        assert_eq!(flag(&adapter.with_session_settings(launch())), "the wording it opened with");
        // A launch that already carried one keeps exactly one.
        let twice = adapter.with_session_settings(adapter.with_session_settings(launch()));
        flag(&twice);
    }

    /// docs/85 10.2: AGENTS.md reaches Claude exactly once, without a file
    /// written in the repo — from 2.1.277 through the CLI's own plugin
    /// setting (unit-tested only: not verifiable on 2.1.276), below it as a
    /// section of the note when the repo has AGENTS.md and no CLAUDE.md.
    #[test]
    fn agents_md_reaches_claude_exactly_once() {
        let root = scratch_dir("agents-md");
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).expect("repo");
        std::fs::write(repo.join("AGENTS.md"), "Run `make check` before you finish.\n").expect("agents");
        let listing = |dir: &std::path::Path| {
            let mut names: Vec<String> = std::fs::read_dir(dir).expect("reads").flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            names.sort();
            names
        };
        let before = listing(&repo);
        let launch = |id: &str| SessionLaunch { cwd: Some(repo.to_string_lossy().into_owned()), ..argv::argv_for_resume(id, None, None, None) };
        let note_of = |launch: &SessionLaunch| {
            let at = launch.argv.iter().position(|arg| arg == "--append-system-prompt").expect("note");
            launch.argv[at + 1].clone()
        };
        // Below 2.1.277: in the note, once.
        let old = ClaudeCodeAdapter::new("claude-must-never-spawn").with_home(root.join("owner")).with_config_dir(root.join("home"));
        *old.version.lock().expect("version mutex") = Some("2.1.276".into());
        let spawned = old.with_session_settings(launch("agents-old"));
        assert_eq!(note_of(&spawned).matches("make check").count(), 1, "{}", note_of(&spawned));
        assert!(argv::settings_on(&spawned).and_then(|s| s.get("pluginConfigs").cloned()).is_none());
        // From 2.1.277: the setting, once, and the note leaves it out.
        let new = ClaudeCodeAdapter::new("claude-must-never-spawn").with_home(root.join("owner")).with_config_dir(root.join("home"));
        *new.version.lock().expect("version mutex") = Some("2.1.277".into());
        let spawned = new.with_session_settings(launch("agents-new"));
        assert!(!note_of(&spawned).contains("make check"));
        assert_eq!(spawned.argv.iter().filter(|arg| arg.contains("cc-plugin-agents-md@builtin")).count(), 1);
        // A repo with CLAUDE.md as well: the CLI reads that itself.
        std::fs::write(repo.join("CLAUDE.md"), "notes\n").expect("claude md");
        let spawned = old.with_session_settings(launch("agents-both"));
        assert!(!note_of(&spawned).contains("make check"));
        std::fs::remove_file(repo.join("CLAUDE.md")).expect("rm");
        assert_eq!(listing(&repo), before, "nothing is written in the repo");
    }

    /// docs/85 10.6: every spawn carries the token settings — Bash output
    /// cut inline at 12,000 characters, NotebookEdit left out — and only an
    /// account that bills by API key asks for the one-hour prompt cache.
    #[test]
    fn every_spawn_carries_the_token_settings() {
        let root = scratch_dir("token-settings");
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn").with_config_dir(root.join("home"));
        let launch = || argv::argv_for_resume("sess-tokens", None, None, None);
        let spawned = adapter.with_session_settings(launch());
        let settings = argv::settings_on(&spawned).expect("a settings layer");
        assert_eq!(settings["bashOutputMaxChars"], serde_json::json!(12_000));
        let at = spawned.argv.iter().position(|arg| arg == "--disallowedTools").expect("--disallowedTools");
        assert_eq!(spawned.argv[at + 1], "NotebookEdit");
        assert_eq!(spawned.argv.iter().filter(|arg| *arg == "--disallowedTools").count(), 1);
        assert!(!spawned.env.iter().any(|(key, _)| key == argv::PROMPT_CACHE_TTL_ENV), "a subscription has it already");
        adapter.set_bills_by_key(true);
        let spawned = adapter.with_session_settings(adapter.with_session_settings(launch()));
        assert_eq!(
            spawned.env.iter().filter(|(key, value)| key == argv::PROMPT_CACHE_TTL_ENV && value == "1h").count(),
            1,
            "{:?}",
            spawned.env
        );
        assert_eq!(spawned.argv.iter().filter(|arg| *arg == "--disallowedTools").count(), 1, "once, however often built");
    }

    /// docs/82 6.2: `ResumeSession.env` reaches the resumed child too — a
    /// worktree session reopened after a restart keeps its port block.
    #[test]
    fn a_resume_starts_the_child_with_the_sessions_env() {
        let root = scratch_dir("resume-env");
        let (baaz, work) = (root.join("baaz-home"), root.join("work"));
        std::fs::create_dir_all(&work).expect("workspace");
        let id = format!("env-{}", std::process::id());
        let slug = baaz.join("projects").join(history::slug_for_cwd(&work));
        std::fs::create_dir_all(&slug).expect("slug dir");
        std::fs::write(
            slug.join(format!("{id}.jsonl")),
            format!(
                "{{\"type\":\"user\",\"cwd\":\"{}\",\"sessionId\":\"{id}\",\"message\":{{\"role\":\"user\",\"content\":\"hi\"}}}}\n",
                work.display()
            ),
        )
        .expect("transcript");
        let (dir, _) = seed_executable("resume-env", "unused", "");
        let out = dir.join("env.txt");
        let script = dir.join("fake-claude");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s|%s' \"$BAAZ_WORKTREE_PATH\" \"$BAAZ_PORT\" > '{out}.tmp'; mv '{out}.tmp' '{out}'\nexec cat >/dev/null\n",
                out = out.display()
            ),
        )
        .expect("fake writes");
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let adapter = ClaudeCodeAdapter::new(&script.to_string_lossy()).with_config_dir(baaz);
        let ack = adapter.dispatch(Command::ResumeSession {
            request_id: "r-env".into(),
            session_id: id,
            cursor: None,
            metadata_only: false,
            mode: None,
            workspace: Some(work.to_string_lossy().into_owned()),
            env: vec![("BAAZ_WORKTREE_PATH".to_owned(), "/w/s-2".to_owned()), ("BAAZ_PORT".to_owned(), "41020".to_owned())],
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !out.is_file() {
            assert!(std::time::Instant::now() < deadline, "the stand-in never started");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        drop(adapter);
        assert!(matches!(ack, Ok(Ack::Session { .. })), "resumed: {ack:?}");
        assert_eq!(std::fs::read_to_string(&out).expect("recorded"), "/w/s-2|41020");
    }

    /// THE 2026-10-30 IMPORT REOPEN. An imported session (docs/72) is
    /// pinned on its owner home; its resume must start the child IN the
    /// session's own folder (the cwd its transcript records — the record's
    /// workspace) with `CLAUDE_CONFIG_DIR` = that home's `.claude`. The
    /// spawn took the Baaz home for every override and no cwd at all, so
    /// the child looked in the wrong config dir and died "No conversation
    /// found". A Baaz-started session keeps the Baaz home and gains its
    /// folder; a session whose folder is gone still resumes.
    #[test]
    fn an_imported_resume_spawns_in_its_folder_under_the_owner_home() {
        let root = scratch_dir("import-resume");
        let (owner, baaz, work) = (root.join("owner"), root.join("baaz-home"), root.join("cockpit"));
        std::fs::create_dir_all(&work).expect("workspace");
        std::fs::create_dir_all(&baaz).expect("baaz home");
        let id = format!("imp-{}", std::process::id());
        let transcript = |config: &std::path::Path, session: &str, cwd: &std::path::Path| {
            let slug = config.join("projects").join(history::slug_for_cwd(cwd));
            std::fs::create_dir_all(&slug).expect("slug dir");
            let head = format!(
                "{{\"type\":\"queue-operation\"}}\n{{\"type\":\"user\",\"cwd\":\"{}\",\"sessionId\":\"{session}\",\"message\":{{\"role\":\"user\",\"content\":\"hi\"}}}}\n",
                cwd.display()
            );
            std::fs::write(slug.join(format!("{session}.jsonl")), head).expect("transcript");
        };
        let resume = |adapter: &ClaudeCodeAdapter, session: &str| {
            adapter.dispatch(Command::ResumeSession {
                request_id: "r-resume".into(),
                session_id: session.into(),
                cursor: None,
                metadata_only: false,
                mode: None,
                workspace: None,
                env: Vec::new(),
            })
        };

        // Imported: the transcript lives only under the owner home.
        transcript(&owner.join(".claude"), &id, &work);
        let (script, out) = recording_claude("import-resume");
        let adapter = ClaudeCodeAdapter::new(&script.to_string_lossy()).with_config_dir(baaz.clone());
        crate::home::pin_legacy_resume_at(&id, &owner);
        let ack = resume(&adapter, &id);
        let (cwd, config) = seen_by(&out);
        drop(adapter);
        crate::home::clear_legacy_resume(&id);
        assert!(matches!(ack, Ok(Ack::Session { .. })), "resumed: {ack:?}");
        assert_eq!(cwd, work.to_string_lossy(), "the child runs in the session's own folder");
        assert_eq!(config, owner.join(".claude").to_string_lossy(), "and reads the owner home's config dir");

        // Baaz-started: the Baaz home, and its folder too.
        let own = format!("own-{}", std::process::id());
        transcript(&baaz, &own, &work);
        let (script, out) = recording_claude("own-resume");
        let adapter = ClaudeCodeAdapter::new(&script.to_string_lossy()).with_config_dir(baaz.clone());
        let ack = resume(&adapter, &own);
        let (cwd, config) = seen_by(&out);
        drop(adapter);
        assert!(matches!(ack, Ok(Ack::Session { .. })), "resumed: {ack:?}");
        assert_eq!(cwd, work.to_string_lossy());
        assert_eq!(config, baaz.to_string_lossy(), "a Baaz session stays Baaz-homed");

        // The folder is gone: the resume still spawns (from the host's
        // cwd — the CLI finds the transcript by id), never a refusal
        // dressed as a missing program.
        let gone_id = format!("gone-{}", std::process::id());
        transcript(&owner.join(".claude"), &gone_id, &root.join("deleted-folder"));
        let (script, out) = recording_claude("gone-resume");
        let adapter = ClaudeCodeAdapter::new(&script.to_string_lossy()).with_config_dir(baaz.clone());
        crate::home::pin_legacy_resume_at(&gone_id, &owner);
        let ack = resume(&adapter, &gone_id);
        let (_, config) = seen_by(&out);
        drop(adapter);
        crate::home::clear_legacy_resume(&gone_id);
        assert!(matches!(ack, Ok(Ack::Session { .. })), "a missing folder still resumes: {ack:?}");
        assert_eq!(config, owner.join(".claude").to_string_lossy());

        // No transcript anywhere: refused typed before any spawn.
        let missing = format!("missing-{}", std::process::id());
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn").with_config_dir(baaz.clone());
        crate::home::pin_legacy_resume_at(&missing, &owner);
        let error = resume(&adapter, &missing).expect_err("nothing to resume");
        crate::home::clear_legacy_resume(&missing);
        assert_eq!(error.hint(), provider::RefusalHint::HistoryMissing, "typed: {error:?}");
        let _ = std::fs::remove_dir_all(&root);
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
        // docs/85 16.2: `skills/` is a real folder of one link per skill.
        assert!(home.join("skills").is_dir(), "the skills folder is made");
        assert_eq!(std::fs::read_dir(home.join("skills")).expect("reads").count(), 0, "no skills yet");
        // The owner adds a skill later, and one in ~/.agents/skills: the
        // next spawn's re-ensure links both without a restart.
        let skill = |root: std::path::PathBuf, name: &str| {
            std::fs::create_dir_all(root.join(name)).expect("skill dir");
            std::fs::write(root.join(name).join("SKILL.md"), "---\nname: x\n---\n").expect("skill");
        };
        skill(owner.join(".claude").join("skills"), "review");
        skill(owner.join(".agents").join("skills"), "shared");
        adapter.prepare_home(false);
        for name in ["review", "shared"] {
            assert!(
                std::fs::symlink_metadata(home.join("skills").join(name)).expect("meta").file_type().is_symlink(),
                "{name} is linked on the next spawn"
            );
        }
        // A skill that went away loses its link; idempotent otherwise.
        std::fs::remove_dir_all(owner.join(".agents").join("skills").join("shared")).expect("remove");
        adapter.prepare_home(false);
        assert!(std::fs::symlink_metadata(home.join("skills").join("shared")).is_err(), "a stale link goes");
        assert!(home.join("skills").join("review").join("SKILL.md").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_account_is_true_after_a_meter_reading() {
        // A rate-limit reading from the live child is authenticated API
        // traffic observed first-hand: signed in, with the meter label.
        let adapter = ClaudeCodeAdapter::new(NO_CLI);
        let frame = frame::decode_line(meter_line()).expect("meter line decodes");
        adapter.fold.lock().expect("fold mutex").apply(&frame);
        let ack = adapter.dispatch(Command::ReadAccount).expect("account reads");
        match ack {
            Ack::Account { signed_in: true, label: Some(label), .. } => {
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
            .values()
            // The hub's own `mcp_status` queries ride beside every
            // adapter request; these tests answer the adapter's.
            .find(|pending| pending.kind != PendingControlKind::McpStatus)
            .map(|pending| pending.request_id.clone())
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
            wanted_mcp: None,
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
    /// and keeps reporting `auto` (probed). Only once the child CONFIRMS the
    /// switch: until then the session may still be on a model with auto.
    /// The chip waits for the child's report. A switch onto a model with
    /// auto, or one made outside Auto, sends nothing more.
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
            let id = pending_id(&adapter);
            adapter.ingest_child_line(&success_line(&id));
            // The next dispatch settles the confirmed switch.
            let _ = adapter.dispatch(Command::ListModels { session: None });
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
        adapter.hub.pending.lock().expect("pending mutex").clear();
        adapter.fold.lock().expect("fold mutex").note_permission_mode(PermissionMode::PromptUnmatched);
        select("haiku");
        assert_eq!(adapter.sent_control_lines().len(), 4);
    }

    /// The host falls back to Ask on the pick, and the person may pick a
    /// mode before the child's confirmation is settled: either one stands,
    /// so the confirmed switch sends no second mode change and no stale
    /// fallback banner lands beside the mode they chose.
    #[test]
    fn a_mode_requested_after_the_switch_supersedes_the_fallback() {
        for mode in [PermissionMode::PromptUnmatched, PermissionMode::DenyUnmatched] {
            let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
            adapter.attach_session_for_tests("sess-1");
            fold_catalog(&adapter);
            adapter.fold.lock().expect("fold mutex").note_permission_mode(PermissionMode::OnRequest);
            adapter
                .dispatch(Command::SelectModel {
                    request_id: "r-1".into(),
                    session_id: "sess-1".into(),
                    model: "haiku".into(),
                    model_provider: None,
                })
                .expect("a pick lands");
            let id = pending_id(&adapter);
            adapter.ingest_child_line(&success_line(&id));
            adapter
                .dispatch(Command::SelectApprovalMode {
                    request_id: "r-2".into(),
                    session_id: "sess-1".into(),
                    mode,
                })
                .expect("the mode pick lands");
            let _ = adapter.dispatch(Command::ListModels { session: None });
            let modes: Vec<String> = adapter
                .sent_control_lines()
                .into_iter()
                .filter(|line| line.contains("set_permission_mode"))
                .collect();
            assert!(modes.len() <= 1 && !modes.iter().any(|line| line.contains("\"manual\"") && mode != PermissionMode::PromptUnmatched), "{mode:?}: {modes:?}");
            let cards = control_cards(&drain_events(&adapter));
            assert!(cards.iter().all(|(kind, _)| kind != CONTROL_MODE_FALLBACK_CARD), "{mode:?}: {cards:?}");
        }
    }

    /// A switch onto a model without auto that the child REFUSES leaves the
    /// session on its old model — which has auto — so Auto stays.
    #[test]
    fn a_refused_switch_never_leaves_auto() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        fold_catalog(&adapter);
        *adapter.model.lock().expect("model mutex") = Some("sonnet".into());
        adapter.fold.lock().expect("fold mutex").note_permission_mode(PermissionMode::OnRequest);
        adapter
            .dispatch(Command::SelectModel {
                request_id: "r-1".into(),
                session_id: "sess-1".into(),
                model: "haiku".into(),
                model_provider: None,
            })
            .expect("a pick lands");
        let id = pending_id(&adapter);
        adapter.ingest_child_line(&error_line(&id, "nope"));
        let _ = adapter.dispatch(Command::ListModels { session: None });
        let lines = adapter.sent_control_lines();
        assert!(lines.iter().all(|line| !line.contains("set_permission_mode")), "{lines:?}");
        let cards = control_cards(&drain_events(&adapter));
        assert!(cards.iter().all(|(kind, _)| kind != CONTROL_MODE_FALLBACK_CARD), "{cards:?}");
    }

    /// A pick into Auto still waiting on its relaunch (out of a launched
    /// Read-only) becomes a relaunch into Ask when a confirmed switch lands
    /// on a model without auto — it never relaunches into Auto anyway.
    #[test]
    fn a_pending_relaunch_into_auto_follows_a_model_without_it() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        fold_catalog(&adapter);
        *adapter.model.lock().expect("model mutex") = Some("sonnet".into());
        *adapter.read_only_layer.lock().expect("read-only mutex") = ReadOnlyLayer::Launched;
        adapter.fold.lock().expect("fold mutex").note_permission_mode(PermissionMode::DenyUnmatched);
        adapter
            .dispatch(Command::SelectApprovalMode {
                request_id: "m-1".into(),
                session_id: "sess-1".into(),
                mode: PermissionMode::OnRequest,
            })
            .expect("admitted");
        assert_eq!(*adapter.mode_relaunch_due.lock().expect("mode relaunch mutex"), Some(PermissionMode::OnRequest));
        adapter
            .dispatch(Command::SelectModel {
                request_id: "r-1".into(),
                session_id: "sess-1".into(),
                model: "haiku".into(),
                model_provider: None,
            })
            .expect("a pick lands");
        let id = pending_id(&adapter);
        adapter.ingest_child_line(&success_line(&id));
        let _ = drain_events(&adapter);
        let _ = adapter.dispatch(Command::ListModels { session: None });
        assert_eq!(
            *adapter.mode_relaunch_due.lock().expect("mode relaunch mutex"),
            Some(PermissionMode::PromptUnmatched),
            "the relaunch goes into Ask"
        );
        let events = drain_events(&adapter);
        assert!(
            events.iter().any(|event| matches!(
                event,
                ProviderEvent::ApprovalModeChanged { mode: PermissionMode::PromptUnmatched, .. }
            )),
            "the chip follows the converted pick: {events:?}"
        );
        assert!(control_cards(&events).iter().any(|(kind, _)| kind == CONTROL_MODE_FALLBACK_CARD));
        let lines = adapter.sent_control_lines();
        assert!(lines.iter().all(|line| !line.contains("set_permission_mode")), "nothing live: {lines:?}");
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
            env: Vec::new(),
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

    /// docs/81 B PM-14: an ask the replaced child raised is gone after a
    /// relaunch — a late decision for it is refused, never written to the
    /// new child under the old request id.
    #[test]
    fn relaunch_drops_the_old_childs_asks() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        adapter.ingest_child_line(
            r#"{"type":"control_request","request_id":"req-old","request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"toolu_1","input":{"command":"ls"}}}"#,
        );
        assert_eq!(adapter.fold.lock().expect("fold").pending_approvals().len(), 1, "setup: the ask waits");
        let launch = SessionLaunch {
            argv: Vec::new(),
            cwd: None,
            session_id: "sess-1".into(),
            model: None,
            effort: None,
            permission_mode: None,
            env: Vec::new(),
        };
        let _ = adapter.relaunch(&launch);
        assert!(adapter.fold.lock().expect("fold").pending_approvals().is_empty(), "the old ask is dropped");
        let error = adapter
            .dispatch(Command::DecideApproval {
                request_id: "d-1".into(),
                session_id: "sess-1".into(),
                approval: "req-old".into(),
                choice: "allow".into(),
                feedback: None,
                stage_token: None,
            })
            .expect_err("a decision for the old child's ask is refused");
        assert!(!format!("{error:?}").is_empty());
        assert!(
            !adapter.sent_control.lock().expect("sent").iter().any(|line| line.contains("req-old")),
            "nothing reached a child under the old id"
        );
    }

    /// docs/81 B PM-15: a withdrawn ask (`control_cancel_request`) is no
    /// longer answerable; an unknown control subtype raises the needs-you
    /// tap as well as its card.
    #[test]
    fn a_withdrawn_ask_leaves_the_queue_and_unknown_controls_are_tapped() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        adapter.ingest_child_line(
            r#"{"type":"control_request","request_id":"req-w","request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"toolu_1","input":{"command":"ls"}}}"#,
        );
        assert_eq!(adapter.fold.lock().expect("fold").pending_approvals().len(), 1);
        adapter.ingest_child_line(r#"{"type":"control_cancel_request","request_id":"req-w"}"#);
        assert!(adapter.fold.lock().expect("fold").pending_approvals().is_empty(), "withdrawn");
        let _ = drain_events(&adapter);
        adapter.ingest_child_line(r#"{"type":"control_request","request_id":"req-x","request":{"subtype":"frobnicate"}}"#);
        let events = drain_events(&adapter);
        assert!(
            events.iter().any(|event| matches!(event, ProviderEvent::ApprovalRequested { approval_id, .. } if approval_id == "req-x")),
            "{events:?}"
        );
    }

    /// A relaunch forgets the replaced child's waiting submissions too: its
    /// end of stream is epoch-gated, so nothing else would, and a stale
    /// entry would name the new child's first turn after an old ack.
    #[test]
    fn relaunch_forgets_submissions_the_old_child_never_answered() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        adapter.fold.lock().expect("fold mutex").note_submission_line("t-old", "u-old");
        let launch = SessionLaunch {
            argv: Vec::new(),
            cwd: None,
            session_id: "sess-1".into(),
            model: None,
            effort: None,
            permission_mode: None,
            env: Vec::new(),
        };
        let _ = adapter.relaunch(&launch);
        assert!(!adapter.fold.lock().expect("fold mutex").has_submissions(), "nothing stale survives");
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
            wanted_mcp: None,
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
                wanted_mcp: None,
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

    /// docs/82 5.5: the Plan toggle turns on the CLI's own `plan` mode over
    /// `set_permission_mode`, and off returns to the mode the chip reads.
    /// The toggle follows the child's reports: entering plan announces it
    /// once and moves no neutral mode; leaving it (the agent's plan was
    /// approved, or the toggle) announces it off.
    #[test]
    fn the_plan_toggle_rides_the_clis_own_plan_mode() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        adapter.fold.lock().expect("fold mutex").set_session_id("sess-1");
        adapter.fold.lock().expect("fold mutex").note_permission_mode(PermissionMode::OnRequest);
        adapter
            .dispatch(Command::SetPlanMode { request_id: "r-1".into(), session_id: "sess-1".into(), on: true })
            .expect("plan on lands");
        adapter
            .dispatch(Command::SetPlanMode { request_id: "r-2".into(), session_id: "sess-1".into(), on: false })
            .expect("plan off lands");
        let modes: Vec<String> = adapter
            .sent_control_lines()
            .iter()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("json"))
            .filter(|value| value["request"]["subtype"] == "set_permission_mode")
            .map(|value| value["request"]["mode"].as_str().unwrap_or_default().to_owned())
            .collect();
        assert_eq!(modes, ["plan", "auto"], "on is `plan`; off is the mode the session was in");
        let plan_events = |events: &[ProviderEvent]| -> Vec<bool> {
            events
                .iter()
                .filter_map(|event| match event {
                    ProviderEvent::PlanModeChanged { session_id, on } if session_id == "sess-1" => Some(*on),
                    _ => None,
                })
                .collect()
        };
        drain_events(&adapter);
        adapter.ingest_child_line(
            r#"{"type":"system","subtype":"status","status":null,"permissionMode":"plan","session_id":"sess-1"}"#,
        );
        adapter.ingest_child_line(
            r#"{"type":"system","subtype":"status","status":null,"permissionMode":"plan","session_id":"sess-1"}"#,
        );
        let entered = drain_events(&adapter);
        assert_eq!(plan_events(&entered), [true], "entering is announced once");
        assert!(mode_events(&entered).is_empty(), "plan moves no neutral mode: the chip keeps its mode");
        adapter.ingest_child_line(
            r#"{"type":"system","subtype":"status","status":null,"permissionMode":"default","session_id":"sess-1"}"#,
        );
        let left = drain_events(&adapter);
        assert_eq!(plan_events(&left), [false], "leaving plan is announced");
        assert_eq!(mode_events(&left), vec![("sess-1".to_owned(), PermissionMode::PromptUnmatched)]);
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
        assert!(
            adapter
                .hub
                .pending
                .lock()
                .expect("pending mutex")
                .values()
                .all(|pending| pending.kind == PendingControlKind::McpStatus),
            "settled but for the MCP query"
        );
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

    /// A child that reports Read-only on its own (the owner's
    /// `defaultMode: dontAsk`) gets the deny layer live before the next
    /// command goes on — `dontAsk` alone still runs owner-allowed tools —
    /// and only once.
    #[test]
    fn a_child_reporting_read_only_on_its_own_gets_the_layer() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        adapter.ingest_child_line(
            r#"{"type":"system","subtype":"status","status":null,"permissionMode":"dontAsk","session_id":"sess-1"}"#,
        );
        let _ = adapter.dispatch(Command::ListModels { session: None });
        let _ = adapter.dispatch(Command::ListModels { session: None });
        let layers: Vec<_> = adapter.sent_control_lines().iter().filter_map(|line| applied_settings(line)).collect();
        assert_eq!(layers, vec![argv::read_only_settings()], "the layer, once");
    }

    /// A relaunch built before the child reported any mode keeps the mode
    /// Baaz last asked for: Read-only never relaunches without its layer.
    #[test]
    fn a_relaunch_before_any_report_keeps_the_requested_mode() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        pick(&adapter, PermissionMode::DenyUnmatched);
        assert_eq!(adapter.fold.lock().expect("fold mutex").permission_mode(), None, "no report yet");
        let launch = adapter.with_session_mode(argv::argv_for_resume("sess-1", None, None, None));
        assert_eq!(launch.permission_mode, Some(PermissionMode::DenyUnmatched));
        assert!(argv::carries_read_only_settings(&launch), "{:?}", launch.argv);
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

    /// The hub's `mcp_status` queries and MCP refreshes, as sent.
    fn mcp_lines(adapter: &ClaudeCodeAdapter) -> Vec<serde_json::Value> {
        adapter
            .hub
            .mcp_sent
            .lock()
            .expect("mcp sent mutex")
            .iter()
            .map(|line| serde_json::from_str(line).expect("json"))
            .collect()
    }

    /// Answer the hub's open `mcp_status` query with `servers`.
    fn answer_mcp_status(adapter: &ClaudeCodeAdapter, servers: serde_json::Value) {
        let query = mcp_lines(adapter)
            .into_iter()
            .rev()
            .find(|line| line["request"]["subtype"] == "mcp_status")
            .expect("an mcp_status query");
        adapter.ingest_child_line(
            &serde_json::json!({
                "type": "control_response",
                "response": {
                    "subtype": "success",
                    "request_id": query["request_id"],
                    "response": {"mcpServers": servers},
                },
            })
            .to_string(),
        );
    }

    /// docs/85 16.1: the spawn handshake asks `get_usage`, and its answer
    /// — matched by request id like every control answer — fills the
    /// account meter before any turn; an answer under a foreign id does not.
    #[test]
    fn the_handshake_asks_get_usage_and_its_answer_fills_the_meter() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        adapter.send_initialize();
        let ask: serde_json::Value = adapter
            .sent_control_lines()
            .iter()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find(|line| line["request"]["subtype"] == "get_usage")
            .expect("a get_usage query at spawn");
        assert_eq!(ask["request"]["skip_behaviors"], true);
        let reply: serde_json::Value = serde_json::from_str(include_str!("../../../fixtures/claude-code/get-usage.json").trim()).unwrap();
        let mut foreign = reply.clone();
        foreign["response"]["request_id"] = "someone-else".into();
        adapter.ingest_child_line(&foreign.to_string());
        assert!(adapter.read_usage().is_none(), "a foreign id fills nothing");
        let mut ours = reply;
        ours["response"]["request_id"] = ask["request_id"].clone();
        adapter.ingest_child_line(&ours.to_string());
        let report = adapter.read_usage().expect("the meter is filled");
        assert_eq!(report.plan.as_deref(), Some("Max"));
        assert_eq!(report.windows.len(), 2);
    }

    fn probe_servers(later: &str) -> serde_json::Value {
        serde_json::json!([
            {"name": "probe", "status": "connected", "tools": [
                {"name": "peek", "annotations": {"readOnly": true}},
                {"name": "poke", "annotations": {}},
            ]},
            {"name": "later", "status": later},
        ])
    }

    fn deny_rules(settings: &serde_json::Value) -> Vec<String> {
        settings["permissions"]["deny"]
            .as_array()
            .expect("deny")
            .iter()
            .filter_map(|rule| rule.as_str().map(str::to_owned))
            .collect()
    }

    /// Read-only reaches the owner's own MCP tools: a child launched in it
    /// asks for its MCP servers right after `initialize`, and the answer
    /// puts a live layer on that denies every acting tool and every server
    /// whose tools are unknown yet — the base rules riding along — while a
    /// read-only-annotated tool stays usable. The next dispatch asks again
    /// and refines the layer once the pending server connects; an answer
    /// that changes nothing sends nothing.
    #[test]
    fn launched_read_only_denies_the_owners_acting_mcp_tools_live() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        let launch = argv::with_permission_mode(
            adapter.launch_for_open("sess-1", None, None).expect("open launch"),
            Some(PermissionMode::DenyUnmatched),
        );
        adapter.note_launch_mode(&launch);
        adapter.send_initialize();
        adapter.attach_session_for_tests("sess-1");
        answer_mcp_status(&adapter, probe_servers("pending"));
        let lines = mcp_lines(&adapter);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[1]["request"]["subtype"], "apply_flag_settings");
        let deny = deny_rules(&lines[1]["request"]["settings"]);
        assert!(deny.contains(&"Bash".to_owned()), "the base rules ride along: {deny:?}");
        assert!(deny.contains(&"mcp__probe__poke".to_owned()) && deny.contains(&"mcp__later".to_owned()));
        assert!(!deny.iter().any(|rule| rule == "mcp__probe__peek" || rule == "mcp__probe"), "{deny:?}");

        // The server connects: the next dispatch's query refines the layer.
        let _ = adapter.dispatch(Command::ListModels { session: None });
        answer_mcp_status(
            &adapter,
            serde_json::json!([{"name": "later", "status": "connected", "tools": [{"name": "send", "annotations": {}}]},
                               {"name": "probe", "status": "connected", "tools": [{"name": "poke"}]}]),
        );
        let lines = mcp_lines(&adapter);
        let refreshed = deny_rules(&lines.last().expect("refresh")["request"]["settings"]);
        assert!(refreshed.contains(&"mcp__later__send".to_owned()) && !refreshed.contains(&"mcp__later".to_owned()));

        // Nothing moved: no further layer.
        let before = mcp_lines(&adapter).len();
        let _ = adapter.dispatch(Command::ListModels { session: None });
        answer_mcp_status(
            &adapter,
            serde_json::json!([{"name": "probe", "status": "connected", "tools": [{"name": "poke"}]},
                               {"name": "later", "status": "connected", "tools": [{"name": "send"}]}]),
        );
        let after = mcp_lines(&adapter);
        assert_eq!(after.len(), before + 1, "only the query: {after:?}");
    }

    /// Outside Read-only an answer applies nothing, but it is remembered:
    /// a live switch into Read-only carries the known MCP denies on its own
    /// layer, and leaving clears them with the rest — a later answer then
    /// applies nothing.
    #[test]
    fn a_live_switch_into_read_only_carries_the_known_mcp_denies() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        adapter.send_initialize();
        answer_mcp_status(&adapter, probe_servers("failed"));
        assert_eq!(mcp_lines(&adapter).len(), 1, "no layer outside Read-only");

        pick(&adapter, PermissionMode::DenyUnmatched);
        let lines = adapter.sent_control_lines();
        // initialize and get_usage come first; the layer is the one apply.
        let layer = lines.iter().find_map(|line| applied_settings(line)).expect("the layer");
        assert_eq!(layer, argv::read_only_settings_with(&["mcp__later".to_owned(), "mcp__probe__poke".to_owned()]));

        pick(&adapter, PermissionMode::PromptUnmatched);
        let cleared = adapter.sent_control_lines();
        assert!(cleared.iter().any(|line| applied_settings(line) == Some(argv::read_only_settings_cleared())));
        let before = mcp_lines(&adapter).len();
        answer_mcp_status(&adapter, probe_servers("pending"));
        assert_eq!(mcp_lines(&adapter).len(), before, "the layer is off: nothing applied");
    }

    /// Attach a scripted stdin to the adapter's hub as a fresh child's:
    /// its writer thread delivers every line the hub or a layer apply
    /// queues, running `hook` on each.
    fn attach_scripted_stdin(
        adapter: &ClaudeCodeAdapter,
        hook: Option<child::testing::LineHook>,
    ) -> (Arc<Mutex<Vec<String>>>, Arc<std::sync::atomic::AtomicBool>) {
        let stdin = child::testing::ScriptedStdin::new(hook);
        let handles = (Arc::clone(&stdin.lines), Arc::clone(&stdin.fail));
        let epoch = adapter.hub.child_epoch.load(std::sync::atomic::Ordering::SeqCst);
        let writer = child::stdin_writer(stdin, &adapter.hub, epoch).expect("writer");
        adapter.hub.attach_stdin(epoch, writer);
        handles
    }

    /// Answer the hub's last `apply_flag_settings` with success.
    fn confirm_last_layer(adapter: &ClaudeCodeAdapter) {
        let apply = mcp_lines(adapter)
            .into_iter()
            .rev()
            .find(|line| line["request"]["subtype"] == "apply_flag_settings")
            .expect("a layer apply");
        adapter.ingest_child_line(&success_line(apply["request_id"].as_str().expect("id")));
    }

    /// A refresh whose write fails is never counted as held: the failure
    /// refuses it at once (banner, plus a relaunch carrying the layer),
    /// and the next identical answer sends the same denies again. Only
    /// the child's `success` makes them held — after that an identical
    /// answer sends nothing.
    #[test]
    fn a_refresh_that_never_reached_the_child_is_sent_again() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        adapter.fold.lock().expect("fold mutex").set_session_id("sess-1");
        let launch = argv::with_permission_mode(
            argv::argv_for_resume("sess-1", None, None, None),
            Some(PermissionMode::DenyUnmatched),
        );
        adapter.hub.begin_child();
        adapter.note_launch_mode(&launch);
        let (written, fail) = attach_scripted_stdin(&adapter, None);
        adapter.hub.query_mcp_status();
        assert!(child::testing::wait_until(|| written.lock().expect("lines").len() == 1), "the query reached the child");
        fail.store(true, std::sync::atomic::Ordering::SeqCst);
        answer_mcp_status(&adapter, probe_servers("pending"));
        let applies = || mcp_lines(&adapter).iter().filter(|line| line["request"]["subtype"] == "apply_flag_settings").count();
        assert_eq!(applies(), 1);
        let mut cards = Vec::new();
        assert!(
            child::testing::wait_until(|| {
                cards.extend(control_cards(&drain_events(&adapter)));
                cards.iter().any(|(kind, text)| kind == CONTROL_MODE_REJECTED_CARD && text.contains("could not write"))
            }),
            "the failed write refuses the refresh: {cards:?}"
        );
        assert!(adapter.hub.mcp.lock().expect("mcp").in_flight.is_none());
        assert!(adapter.hub.mcp.lock().expect("mcp").confirmed.as_deref() == Some(&[][..]), "still only the launch layer");
        assert!(adapter.hub.take_effort_relaunch().is_some(), "a relaunch carrying the layer is owed");

        // A pipe that broke stays broken; the relaunched child's writer
        // takes lines again. The same answer: sent again, and it lands.
        let (lines, _) = attach_scripted_stdin(&adapter, None);
        adapter.hub.query_mcp_status();
        answer_mcp_status(&adapter, probe_servers("pending"));
        assert_eq!(applies(), 2, "an identical answer resends what never landed");
        confirm_last_layer(&adapter);
        let held = adapter.hub.mcp.lock().expect("mcp").confirmed.clone();
        assert_eq!(held, Some(vec!["mcp__later".to_owned(), "mcp__probe__poke".to_owned()]));
        adapter.hub.query_mcp_status();
        answer_mcp_status(&adapter, probe_servers("pending"));
        assert_eq!(applies(), 2, "held: nothing more to send");
        assert!(child::testing::wait_until(|| lines.lock().expect("lines").iter().any(|line| line.contains("mcp__probe__poke"))));
    }

    /// Leaving a live Read-only is one step against the pump: an
    /// `mcp_status` answer that folds while the clear is being written —
    /// here, from inside the write itself — sees the layer already off and
    /// sends nothing, so the last layer line the child gets is the clear.
    /// (Before, the answer re-applied the layer after the clear.) Entering
    /// is the mirror: the answer carries into the new layer.
    #[test]
    fn an_mcp_answer_racing_a_mode_pick_never_lands_on_the_wrong_side() {
        let adapter = Arc::new(ClaudeCodeAdapter::new("claude-must-never-spawn"));
        adapter.attach_session_for_tests("sess-1");
        adapter.hub.begin_child();
        let racing = Arc::clone(&adapter);
        let hook: child::testing::LineHook = Box::new(move |line: &str| {
            let value: serde_json::Value = serde_json::from_str(line).expect("json");
            let settings = &value["request"]["settings"];
            if value["request"]["subtype"] == "apply_flag_settings" && settings["permissions"].is_null() {
                // The pump lands an answer exactly as the clear goes out.
                racing.hub.query_mcp_status();
                answer_mcp_status(&racing, probe_servers("pending"));
            }
        });
        let (lines, _) = attach_scripted_stdin(&adapter, Some(hook));
        pick(&adapter, PermissionMode::DenyUnmatched);
        answer_mcp_status(&adapter, probe_servers("failed"));
        // The clear's write — the hook's racing answer included — is
        // done when the pick returns: it waits on the write.
        pick(&adapter, PermissionMode::PromptUnmatched);
        let written = lines.lock().expect("lines").clone();
        let layers: Vec<serde_json::Value> = written
            .iter()
            .filter_map(|line| applied_settings(line))
            .collect();
        assert_eq!(layers.last(), Some(&argv::read_only_settings_cleared()), "{layers:?}");
        let sent_layers = mcp_lines(&adapter)
            .iter()
            .filter(|line| line["request"]["subtype"] == "apply_flag_settings")
            .count();
        assert_eq!(sent_layers, 1, "only the refresh while on; none after the clear");
        assert!(!adapter.hub.mcp.lock().expect("mcp").layer_on);
    }

    /// The first Read-only turn with the owner's own MCP servers waits for
    /// the child to confirm the layer: the launch applies every MCP tool
    /// denied (`mcp__*`) live at once; the hold waits for the
    /// `mcp_status` answer and for the apply carrying its denies; and with
    /// no layer confirmed in time the turn is refused, never sent open.
    #[test]
    fn a_read_only_turn_waits_for_the_mcp_layer_and_fails_closed() {
        let adapter = Arc::new(ClaudeCodeAdapter::new("claude-must-never-spawn"));
        adapter.attach_session_for_tests("sess-1");
        adapter.hub.begin_child();
        let (_lines, _) = attach_scripted_stdin(&adapter, None);
        // An owner-MCP launch (no `--strict-mcp-config`), Read-only.
        let launch = argv::with_permission_mode(
            argv::without_strict_mcp(argv::argv_for_resume("sess-1", None, None, None)),
            Some(PermissionMode::DenyUnmatched),
        );
        adapter.note_launch_mode(&launch);
        adapter.cover_launched_mcp();
        let first = applied_settings(&adapter.sent_control_lines()[0]).expect("the live layer");
        assert!(deny_rules(&first).contains(&MCP_WILDCARD_DENY.to_owned()), "{first}");

        // Nothing confirmed: the hold refuses the turn.
        let refused = adapter.hub.await_mcp_layer(std::time::Duration::from_millis(30));
        assert!(refused.is_err(), "{refused:?}");

        // The wildcard confirmed, the answer and its apply still open: the
        // hold waits for both, then lets the turn go.
        let wildcard = serde_json::from_str::<serde_json::Value>(&adapter.sent_control_lines()[0]).expect("json");
        adapter.ingest_child_line(&success_line(wildcard["request_id"].as_str().expect("id")));
        let waiter = {
            let adapter = Arc::clone(&adapter);
            std::thread::spawn(move || {
                let started = Instant::now();
                (adapter.hub.await_mcp_layer(std::time::Duration::from_secs(4)), started.elapsed())
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(100));
        answer_mcp_status(&adapter, probe_servers("pending"));
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(!waiter.is_finished(), "still waiting on the apply's answer");
        confirm_last_layer(&adapter);
        let (held, waited) = waiter.join().expect("waiter");
        assert!(held.is_ok(), "{held:?}");
        assert!(waited < std::time::Duration::from_secs(3), "released by the answer, not the timeout: {waited:?}");

        // A strict launch serves no owner servers: nothing to hold for.
        let strict = argv::with_permission_mode(
            argv::argv_for_resume("sess-1", None, None, None),
            Some(PermissionMode::DenyUnmatched),
        );
        adapter.hub.begin_child();
        let _ = attach_scripted_stdin(&adapter, None);
        adapter.note_launch_mode(&strict);
        assert!(adapter.hub.await_mcp_layer(std::time::Duration::from_millis(1)).is_ok());
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
                Delta::BlockUpdated { block: Block::Approval { state: ApprovalState::Cancelled { reason: Some(reason) }, .. }, .. }
                    if reason == "Claude Code stopped"
            )),
            "the unanswerable approval card settles not-run, never as the person's denial: {deltas:?}"
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

    /// The interrupt fixture (`fixtures/claude-code/interrupt.jsonl`): a
    /// live turn's frames, the child's `control_response` to the host's
    /// `interrupt` (id `REQUEST_ID`), and the `result` that ends the turn.
    fn interrupt_fixture() -> Vec<String> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/claude-code/interrupt.jsonl");
        std::fs::read_to_string(path).expect("fixture").lines().map(str::to_owned).collect()
    }

    /// An adapter holding the fixture's session with its turn open.
    fn adapter_mid_turn() -> (ClaudeCodeAdapter, Vec<String>) {
        let lines = interrupt_fixture();
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.fold.lock().expect("fold mutex").set_live(true);
        adapter.attach_session_for_tests("704fd676-d199-410c-a1d2-03bf3efd46d9");
        for line in &lines[..lines.len() - 2] {
            adapter.ingest_child_line(line);
        }
        assert!(adapter.fold.lock().expect("fold mutex").has_live_turn(), "setup: the turn is open");
        (adapter, lines)
    }

    fn stop(adapter: &ClaudeCodeAdapter) -> Result<Ack, ProviderError> {
        adapter.dispatch(Command::InterruptTurn {
            request_id: "i-1".into(),
            session_id: "704fd676-d199-410c-a1d2-03bf3efd46d9".into(),
            turn: None,
            retract: false,
        })
    }

    /// docs/82 0.8: Stop sends the protocol's `interrupt`; the child
    /// answers and ends the turn, which settles Interrupted (not Failed),
    /// and the child is kept.
    #[test]
    fn stop_interrupts_through_the_control_protocol() {
        let (mut adapter, lines) = adapter_mid_turn();
        adapter.set_interrupt_grace(std::time::Duration::from_secs(5));
        let adapter = std::sync::Arc::new(adapter);
        let _ = drain_events(&adapter);
        let child = {
            let adapter = adapter.clone();
            std::thread::spawn(move || {
                // The child answers the request it was sent, then ends the turn.
                let request_id = loop {
                    let sent = adapter.sent_control.lock().expect("sent").clone();
                    if let Some(line) = sent.iter().find(|line| line.contains("\"interrupt\"")) {
                        let value: serde_json::Value = serde_json::from_str(line).expect("json");
                        break value["request_id"].as_str().expect("id").to_owned();
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                };
                adapter.ingest_child_line(&lines[lines.len() - 2].replace("REQUEST_ID", &request_id));
                adapter.ingest_child_line(&lines[lines.len() - 1]);
            })
        };
        assert_eq!(stop(&adapter).expect("Stop is accepted"), Ack::Accepted);
        child.join().expect("child thread");
        assert!(!adapter.fold.lock().expect("fold mutex").has_live_turn(), "the turn ended");
        assert!(!adapter.hub.child_lost.load(std::sync::atomic::Ordering::SeqCst), "the child is kept");
        let events = drain_events(&adapter);
        assert!(
            events.iter().any(|event| matches!(event, ProviderEvent::TurnSettled { outcome: provider::facts::TurnOutcome::Interrupted, .. })),
            "settled Interrupted: {events:?}"
        );
        assert!(
            !events.iter().any(|event| matches!(event, ProviderEvent::TurnSettled { outcome: provider::facts::TurnOutcome::Failed { .. }, .. })),
            "never Failed: {events:?}"
        );
    }

    /// docs/82 0.8, the fallback: a child that does not end the turn within
    /// the grace is ended; the turn closes Interrupted with no error card
    /// and no lost-connection report, and the next send resumes.
    #[test]
    fn stop_ends_the_child_when_the_interrupt_is_not_honoured() {
        let (mut adapter, _) = adapter_mid_turn();
        adapter.set_interrupt_grace(std::time::Duration::from_millis(60));
        let _ = drain_events(&adapter);
        assert_eq!(stop(&adapter).expect("Stop is accepted"), Ack::Accepted);
        assert!(
            adapter.sent_control.lock().expect("sent").iter().any(|line| line.contains("\"interrupt\"")),
            "the interrupt was asked for first"
        );
        assert!(!adapter.fold.lock().expect("fold mutex").has_live_turn(), "the turn is closed");
        let events = drain_events(&adapter);
        let finished = events.iter().any(|event| matches!(event, ProviderEvent::Deltas { deltas, .. } if deltas.iter().any(|d| matches!(d, Delta::TurnFinished { .. }))));
        assert!(finished, "the open turn finished: {events:?}");
        assert!(
            events.iter().any(|event| matches!(event, ProviderEvent::TurnSettled { outcome: provider::facts::TurnOutcome::Interrupted, .. })),
            "settled Interrupted: {events:?}"
        );
        assert!(!events.iter().any(|event| matches!(event, ProviderEvent::ConnectionLost { .. })), "not a loss: {events:?}");
        let carded = events.iter().any(|event| matches!(event, ProviderEvent::Deltas { deltas, .. } if deltas.iter().any(|d| matches!(d, Delta::BlockAdded { block: aui_protocol::Block::Generic { kind, .. }, .. } if kind == crate::CHILD_EXITED_CARD))));
        assert!(!carded, "no child-exited card for the person's own stop");
        assert!(
            adapter.revive_launch("704fd676-d199-410c-a1d2-03bf3efd46d9").is_some(),
            "the next send resumes the session"
        );
    }

    /// docs/81 B PM-3: a fold that panics on the pump ends the session like
    /// a lost child — the open turn closes, ConnectionLost is reported —
    /// instead of reading Working forever, and the poisoned lock is usable.
    #[test]
    fn a_panicking_fold_ends_the_session_cleanly() {
        let (adapter, lines) = adapter_mid_turn();
        let _ = drain_events(&adapter);
        let epoch = adapter.hub.begin_child();
        let mut feed = lines[..2].join("\n");
        feed.push('\n');
        feed.push_str(crate::controls::TEST_PANIC_LINE);
        feed.push('\n');
        // The turn opened before this child; reopen it on this epoch.
        let pump = crate::child::spawn_pump(std::io::Cursor::new(feed.into_bytes()), &adapter.hub, epoch, None)
            .expect("pump");
        pump.join().expect("the pump thread ends without panicking");
        assert!(adapter.fold.lock().is_ok(), "the poison is cleared");
        let events = drain_events(&adapter);
        assert!(
            events.iter().any(|event| matches!(event, ProviderEvent::ConnectionLost { reason } if reason.contains("internal error"))),
            "the session ends as a lost connection: {events:?}"
        );
        assert!(!adapter.fold.lock().expect("fold").has_live_turn(), "no turn is left Working");
        assert!(
            adapter.revive_launch("704fd676-d199-410c-a1d2-03bf3efd46d9").is_some(),
            "the next send revives the session"
        );
    }

    /// Stop with nothing running is a no-op, not an error.
    #[test]
    fn stop_with_no_turn_running_does_nothing() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("704fd676-d199-410c-a1d2-03bf3efd46d9");
        assert_eq!(stop(&adapter).expect("accepted"), Ack::Accepted);
        assert!(adapter.sent_control.lock().expect("sent").is_empty(), "nothing to interrupt");
    }

    /// A scratch dir unique to one test: removed first, created empty.
    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir.canonicalize().expect("canonical scratch dir")
    }

    /// docs/82 8.9: with "Use my own MCP servers" on, every launch — the
    /// open, a resume, and a relaunch that resumes — carries the person's
    /// own servers (from a fixture `~/.claude.json`, never the real one) as
    /// a second `--mcp-config` beside the bridge's, once; off, or with no
    /// source, nothing of theirs rides and the bridge is never lost.
    #[test]
    fn the_own_servers_switch_passes_the_persons_servers_on_every_launch() {
        let root = scratch_dir("own-mcp");
        let owner = root.join("owner-home");
        std::fs::create_dir_all(&owner).expect("owner home");
        std::fs::write(owner.join(".claude.json"), include_str!("../../../fixtures/claude-code/mcp-claude.json")).expect("fixture");
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.set_terminal_relay(TerminalRelay {
            bridge: PathBuf::from("/tmp/baaz/bin/mcp-bridge"),
            socket: PathBuf::from("/tmp/baaz/run/terminal-9.sock"),
            config_dir: root.join("mcp"),
        });
        adapter.set_owner_mcp_source(Some(owner.join(".claude.json")));
        let configs = |argv: &[String]| -> Vec<String> {
            argv.windows(2).filter(|pair| pair[0] == "--mcp-config").map(|pair| pair[1].clone()).collect()
        };
        // Off: the bridge alone, strict.
        let open = adapter.launch_for_open("req-1", Some("/work"), None).expect("open");
        assert_eq!(configs(&open.argv).len(), 1, "off: the bridge alone: {:?}", open.argv);
        assert!(open.argv.contains(&"--strict-mcp-config".to_owned()));
        // On: the bridge and the person's servers, no strict.
        adapter.set_use_own_mcp(true);
        let open = adapter.launch_for_open("req-1", Some("/work"), None).expect("open");
        let files = configs(&open.argv);
        assert_eq!(files.len(), 2, "bridge plus own: {:?}", open.argv);
        assert!(!open.argv.contains(&"--strict-mcp-config".to_owned()));
        let own: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&files[1]).expect("own file")).expect("json");
        let mut names: Vec<&String> = own["mcpServers"].as_object().expect("servers").keys().collect();
        names.sort();
        assert_eq!(names, ["filesystem", "linear", "local-only"], "user scope plus /work's local scope, never baaz");
        // A resume carries it too.
        let resume = adapter.launch_for_resume("req-1").expect("resume");
        assert_eq!(configs(&resume.argv).len(), 2, "resume: {:?}", resume.argv);
        // A relaunch built without any config gains the bridge and the
        // person's file, once each.
        adapter.attach_session_for_tests("req-1");
        let bare = argv::argv_for_resume("req-1", None, None, None);
        let (relaunch, _) = adapter.relaunch_launch(&bare).expect("built");
        let files = configs(&relaunch.argv);
        assert_eq!(files.len(), 2, "relaunch: bridge plus own: {:?}", relaunch.argv);
        assert!(files.iter().any(|f| f.ends_with("req-1.json")) && files.iter().any(|f| f.ends_with("req-1-own.json")));
        let (again, _) = adapter.relaunch_launch(&relaunch).expect("built");
        assert_eq!(configs(&again.argv).len(), 2, "never twice: {:?}", again.argv);
        // No source (an adapter the factory did not set up): nothing read.
        adapter.set_owner_mcp_source(None);
        let open = adapter.launch_for_open("req-2", Some("/work"), None).expect("open");
        assert_eq!(configs(&open.argv).len(), 1, "no source, no file: {:?}", open.argv);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// THE 2026-10-17 EXIT. A draft launched Read-only (`--settings`
    /// layer), switched to another mode before its first turn, owes a
    /// relaunch — and the CLI has stored nothing yet, so `--resume` exits
    /// "No conversation found with session ID" before the turn. The
    /// relaunch opens the session fresh under its own id, in its
    /// workspace; once a transcript exists it resumes, still in the
    /// workspace.
    #[test]
    fn a_relaunch_before_the_first_turn_opens_fresh_instead_of_resuming() {
        let root = scratch_dir("fresh-relaunch");
        let (config, work) = (root.join("home"), root.join("work"));
        std::fs::create_dir_all(&work).expect("workspace");
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn").with_config_dir(config.clone());
        adapter.attach_session_for_tests("sess-1");
        *adapter.workspace.lock().expect("workspace mutex") = Some(work.clone());
        let open = argv::argv_for_open("sess-1", Some(&work.to_string_lossy()), None, None, None);
        adapter.note_launch_mode(&argv::with_permission_mode(open, Some(PermissionMode::DenyUnmatched)));
        pick(&adapter, PermissionMode::PromptUnmatched);
        let owed = adapter.take_mode_relaunch("sess-1").expect("leaving launched Read-only relaunches");
        assert!(owed.argv.contains(&"--resume".to_owned()), "built as a resume: {:?}", owed.argv);
        let (fresh, _) = adapter.relaunch_launch(&owed).expect("built");
        assert!(!fresh.argv.contains(&"--resume".to_owned()), "never --resume a session never stored: {:?}", fresh.argv);
        let at = fresh.argv.iter().position(|arg| arg == "--session-id").expect("--session-id");
        assert_eq!(fresh.argv[at + 1], "sess-1", "the same id the open minted");
        assert_eq!(fresh.cwd.as_deref(), Some(work.to_string_lossy().as_ref()));
        let at = fresh.argv.iter().position(|arg| arg == "--permission-mode").expect("mode flag");
        assert_eq!(fresh.argv[at + 1], "manual");
        assert!(!argv::carries_read_only_settings(&fresh));

        // The first turn stored the transcript: a later relaunch resumes it.
        let slug = config.join("projects").join(history::slug_for_cwd(&work));
        std::fs::create_dir_all(&slug).expect("slug dir");
        std::fs::write(slug.join("sess-1.jsonl"), "{}\n").expect("transcript");
        let (resumed, _) = adapter.relaunch_launch(&owed).expect("built");
        let at = resumed.argv.iter().position(|arg| arg == "--resume").expect("--resume once stored");
        assert_eq!(resumed.argv[at + 1], "sess-1");
        assert!(!resumed.argv.contains(&"--session-id".to_owned()));
        assert_eq!(resumed.cwd.as_deref(), Some(work.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// No launch carries `--permission-mode auto` on a model without Auto:
    /// the launch's own `--model`, else the recorded pick, decides — by
    /// its catalog row when one is known, by the Haiku family before —
    /// and the launch goes out in Ask with the notice saying why.
    #[test]
    fn auto_never_launches_on_a_model_without_it() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        let auto = |model: Option<&str>| {
            argv::with_permission_mode(
                argv::argv_for_open("sess-1", None, model, None, None),
                Some(PermissionMode::OnRequest),
            )
        };
        let mode_of = |launch: &SessionLaunch| {
            let at = launch.argv.iter().position(|arg| arg == "--permission-mode").expect("mode flag");
            launch.argv[at + 1].clone()
        };
        let (kept, notice) = adapter.launch_off_unsupported_auto(auto(Some("sonnet")));
        assert_eq!((mode_of(&kept).as_str(), notice), ("auto", None), "Sonnet keeps Auto");
        let (kept, notice) = adapter.launch_off_unsupported_auto(auto(None));
        assert_eq!((mode_of(&kept).as_str(), notice), ("auto", None), "the CLI's default keeps Auto");
        for model in ["haiku", "claude-haiku-4-5-20251001"] {
            let (asked, notice) = adapter.launch_off_unsupported_auto(auto(Some(model)));
            assert_eq!(mode_of(&asked), "manual", "{model}: {:?}", asked.argv);
            assert_eq!(asked.permission_mode, Some(PermissionMode::PromptUnmatched));
            assert_eq!(
                notice.as_deref(),
                Some("Auto is not available for Haiku; Claude Code will ask before acting.")
            );
        }
        // No `--model` on the launch: the recorded pick (a remembered
        // model sent with `set_model`) decides — the mode relaunch above
        // went out `--model haiku --permission-mode auto` live.
        *adapter.model.lock().expect("model mutex") = Some("haiku".into());
        let (asked, _) = adapter.launch_off_unsupported_auto(auto(None));
        assert_eq!(mode_of(&asked), "manual");
        assert_eq!(*adapter.requested_mode.lock().expect("requested mode mutex"), Some(PermissionMode::PromptUnmatched));
        let (relaunch, notice) = adapter
            .relaunch_launch(&argv::with_permission_mode(
                argv::argv_for_resume("sess-1", Some("haiku"), None, None),
                Some(PermissionMode::OnRequest),
            ))
            .expect("built");
        assert_eq!(mode_of(&relaunch), "manual", "{:?}", relaunch.argv);
        assert!(notice.is_some());
        // Once the catalog is folded, its rows speak: Sonnet's says Auto.
        fold_catalog(&adapter);
        *adapter.model.lock().expect("model mutex") = Some("sonnet".into());
        let (kept, _) = adapter.launch_off_unsupported_auto(auto(None));
        assert_eq!(mode_of(&kept), "auto");
    }

    /// A confirmed switch onto Haiku while in Auto leaves for Ask even
    /// before the child's catalog answered — the draft-open case, where
    /// the remembered model's `set_model` goes out at the landing, ahead
    /// of the `initialize` answer.
    #[test]
    fn a_confirmed_haiku_switch_leaves_auto_without_a_catalog() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        adapter.fold.lock().expect("fold mutex").set_session_id("sess-1");
        adapter.fold.lock().expect("fold mutex").note_permission_mode(PermissionMode::OnRequest);
        adapter
            .dispatch(Command::SelectModel {
                request_id: "r-1".into(),
                session_id: "sess-1".into(),
                model: "haiku".into(),
                model_provider: None,
            })
            .expect("a pick lands");
        adapter.ingest_child_line(&success_line(&pending_id(&adapter)));
        let _ = adapter.dispatch(Command::ListModels { session: None });
        let modes: Vec<String> =
            adapter.sent_control_lines().into_iter().filter(|line| line.contains("set_permission_mode")).collect();
        assert_eq!(modes.len(), 1, "{modes:?}");
        assert!(modes[0].contains("\"manual\""), "{modes:?}");
        let cards = control_cards(&drain_events(&adapter));
        assert!(cards.iter().any(|(kind, _)| kind == CONTROL_MODE_FALLBACK_CARD), "{cards:?}");
    }

    /// The fake child for the exit path: `/bin/sh` printing what the CLI
    /// prints on stderr, then exiting before any turn. The lost-connection
    /// reason quotes it; a child that exits silently keeps the plain one.
    #[test]
    fn a_child_exit_names_the_clis_stderr_reason() {
        for (script, expected) in [
            (
                "echo 'No conversation found with session ID: sess-1' >&2; exit 1",
                "the Claude Code process exited: No conversation found with session ID: sess-1; send again to resume the session"
                    .to_owned(),
            ),
            ("exit 0", crate::child::CHILD_EXITED_REASON.to_owned()),
        ] {
            let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
            let launch = SessionLaunch {
                argv: vec!["-c".into(), script.into()],
                cwd: None,
                session_id: "sess-1".into(),
                model: None,
                effort: None,
                permission_mode: None,
                env: Vec::new(),
            };
            let _child = crate::child::RunningChild::spawn_legacy("/bin/sh", &launch, &adapter.hub).expect("sh runs");
            let events = adapter.events();
            let lost = loop {
                match events.recv_timeout(std::time::Duration::from_secs(5)).expect("the exit is reported") {
                    ProviderEvent::ConnectionLost { reason } => break reason,
                    _ => continue,
                }
            };
            assert_eq!(lost, expected, "{script}");
        }
    }

    /// THE KILLED CHILD (docs/82 5.1). A stand-in child SIGKILLed under a
    /// live session: the adapter reports the connection lost, and nothing
    /// it holds stays locked — a send right after is answered at once, and
    /// the hang-up returns promptly.
    #[test]
    fn a_killed_child_is_reported_lost_and_holds_no_lock() {
        let mut adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        let launch = SessionLaunch {
            argv: vec!["-c".into(), "echo $$ >&2; exec cat".into()],
            cwd: None,
            session_id: "sess-1".into(),
            model: None,
            effort: None,
            permission_mode: None,
            env: Vec::new(),
        };
        let child = crate::child::RunningChild::spawn_legacy("/bin/sh", &launch, &adapter.hub).expect("sh runs");
        *adapter.child.lock().expect("child mutex") = Some(child);
        let pid = {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if let Some(pid) = adapter.hub.stderr.lines().first().and_then(|line| line.trim().parse::<i32>().ok()) {
                    break pid;
                }
                assert!(std::time::Instant::now() < deadline, "the stand-in never said its pid");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        let events = adapter.events();
        // SAFETY: SIGKILL to the stand-in this test just spawned.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        loop {
            match events.recv_timeout(std::time::Duration::from_secs(5)).expect("the loss is reported") {
                ProviderEvent::ConnectionLost { .. } => break,
                _ => continue,
            }
        }
        let started = std::time::Instant::now();
        let _ = adapter.dispatch(Command::SubmitInput {
            request_id: "t-1".into(),
            session_id: "sess-1".into(),
            parts: vec![provider::SubmissionPart::Text("still there?".into())],
            display_text: None,
            effort: None,
        });
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "the send was answered, not stuck");
        let started = std::time::Instant::now();
        adapter.shutdown();
        assert!(started.elapsed() < std::time::Duration::from_secs(2), "the hang-up returned");
    }

    /// The person's message is on screen from the submit, not from the
    /// CLI's echo: a child that never echoes (it died) still leaves it, the
    /// echo that does come adds no second bubble, and a line that never
    /// reached any child takes its bubble back down.
    #[test]
    fn the_sent_message_bubbles_on_the_submit() {
        use aui_protocol::{Delta, Turn};
        let user_turns = |events: &[ProviderEvent]| -> Vec<(String, String)> {
            events
                .iter()
                .flat_map(|event| match event {
                    ProviderEvent::Deltas { deltas, .. } => deltas.clone(),
                    _ => Vec::new(),
                })
                .filter_map(|delta| match delta {
                    Delta::TurnStarted { turn: Turn::User { id, text, .. } } => Some((id, text)),
                    _ => None,
                })
                .collect()
        };
        let removed = |events: &[ProviderEvent]| -> Vec<String> {
            events
                .iter()
                .flat_map(|event| match event {
                    ProviderEvent::Deltas { deltas, .. } => deltas.clone(),
                    _ => Vec::new(),
                })
                .filter_map(|delta| match delta {
                    Delta::TurnRemoved { turn_id } => Some(turn_id),
                    _ => None,
                })
                .collect()
        };
        let submit = |adapter: &ClaudeCodeAdapter| {
            adapter.dispatch(Command::SubmitInput {
                request_id: "t-1".into(),
                session_id: "sess-1".into(),
                parts: vec![provider::SubmissionPart::Text("say HI".into())],
                display_text: None,
                effort: None,
            })
        };

        // No child to take it: the bubble goes up, then comes back down.
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        submit(&adapter).expect_err("no child");
        let events = drain_events(&adapter);
        let shown = user_turns(&events);
        assert_eq!(shown.len(), 1, "{events:?}");
        assert_eq!(shown[0].1, "say HI");
        assert_eq!(removed(&events), vec![shown[0].0.clone()]);

        // `cat` echoes the line back exactly as `--replay-user-messages`
        // does, uuid and all — it folds as the echo — so one bubble, from
        // the submit.
        assert!(matches!(
            frame::decode_line(&argv::user_content_line("say HI", &[], Some("u-1"))),
            Ok(frame::Frame::UserText { uuid, .. }) if uuid == "u-1"
        ));
        let mut adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-1");
        adapter.fold.lock().expect("fold mutex").set_session_id("sess-1");
        let launch = SessionLaunch {
            argv: Vec::new(),
            cwd: None,
            session_id: "sess-1".into(),
            model: None,
            effort: None,
            permission_mode: None,
            env: Vec::new(),
        };
        let cat = crate::child::RunningChild::spawn_legacy("/bin/cat", &launch, &adapter.hub).expect("cat runs");
        *adapter.child.lock().expect("child mutex") = Some(cat);
        let events = adapter.events();
        submit(&adapter).expect("cat takes the line");
        let first = events.recv_timeout(std::time::Duration::from_secs(5)).expect("the bubble");
        let shown = user_turns(std::slice::from_ref(&first));
        assert_eq!(shown.len(), 1, "the submit's own event is the bubble: {first:?}");
        let echoed = adapter.fold.lock().expect("fold mutex").has_submissions();
        assert!(echoed, "still waiting on its turn");
        // Let the echo fold, then hang up.
        std::thread::sleep(std::time::Duration::from_millis(300));
        adapter.shutdown();
        let mut rest = Vec::new();
        while let Ok(event) = events.try_recv() {
            rest.push(event);
        }
        assert!(user_turns(&rest).is_empty(), "the echo adds no second bubble: {rest:?}");
        assert!(removed(&rest).is_empty());
    }
}
