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
pub mod fold;
pub mod frame;
pub mod history;
pub mod terminal;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aui_protocol::Delta;
use crossbeam_channel::{unbounded, Receiver, Sender};
use provider::{
    Ack, CapabilitySet, Command, ConnectInfo, Handshake, PendingApproval, ProviderAdapter,
    ProviderError, ProviderEvent, ProviderId, SessionSummary,
};

pub use caps::{capabilities, claude_version_supported, CLAUDE_VERSION_FLOOR};

use argv::SessionLaunch;
use child::RunningChild;
use fold::{ClaudeFold, UnknownControlRequest};
use frame::ApprovalRequest;

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
    session_id: Mutex<Option<String>>,
    model: Mutex<Option<String>>,
    /// The `--effort` level the running child was launched with, when one
    /// was. Effort is a launch flag — a `SubmitInput` carrying a different
    /// level relaunches the child with `--resume` plus the new flag.
    effort: Mutex<Option<String>>,
    workspace: Mutex<Option<PathBuf>>,
    home_override: Option<PathBuf>,
    connected: Mutex<bool>,
    version: Mutex<Option<String>>,
    /// Where the terminal relay lives, set by the host before the session
    /// commands run: the bridge binary, the socket to point it at, and the
    /// directory (`<support_dir>/mcp`) its per-session config files land
    /// in. `None` means no relay: sessions spawn exactly today's argv.
    terminal: Mutex<Option<TerminalRelay>>,
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
        Self {
            program: program.into(),
            tx,
            rx,
            fold: Arc::new(Mutex::new(ClaudeFold::new())),
            child: Mutex::new(None),
            session_id: Mutex::new(None),
            model: Mutex::new(None),
            effort: Mutex::new(None),
            workspace: Mutex::new(None),
            home_override: None,
            connected: Mutex::new(false),
            version: Mutex::new(None),
            terminal: Mutex::new(None),
        }
    }

    /// Point the terminal relay at the bridge, socket and config directory
    /// before the session commands run. Every open, resume, fork and
    /// effort-swap relaunch then writes its per-session MCP config and
    /// spawns with `--mcp-config <path> --strict-mcp-config`.
    pub fn set_terminal_relay(&self, relay: TerminalRelay) {
        *self.terminal.lock().expect("terminal mutex") = Some(relay);
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
    /// the terminal relay's config file, when one is set. Pure apart from
    /// that one file write — no spawn — so tests drive this without a CLI.
    pub fn launch_for_open(
        &self,
        request_id: &str,
        workspace: Option<&str>,
        model: Option<&str>,
    ) -> Result<SessionLaunch, ProviderError> {
        let mcp = self.mcp_config_for(request_id)?;
        Ok(argv::argv_for_open(request_id, workspace, model, mcp.as_deref(), None))
    }

    /// The resume launch for a stored `session_id`, with the relay's
    /// config file when one is set. No spawn — see [`Self::launch_for_open`].
    pub fn launch_for_resume(&self, session_id: &str) -> Result<SessionLaunch, ProviderError> {
        let mcp = self.mcp_config_for(session_id)?;
        Ok(argv::argv_for_resume(session_id, None, mcp.as_deref(), None))
    }

    /// The fork launch branching `session_id` into `request_id`, with the
    /// relay's config file (for the NEW session) when one is set. No spawn.
    pub fn launch_for_fork(
        &self,
        request_id: &str,
        session_id: &str,
    ) -> Result<SessionLaunch, ProviderError> {
        let mcp = self.mcp_config_for(request_id)?;
        Ok(argv::argv_for_fork(request_id, session_id, None, mcp.as_deref(), None))
    }

    /// Carry the relay onto an effort-swap `launch`: the swap relaunches
    /// with `--resume`, and the resumed child must see the same bridge the
    /// old one did. A launch that already carries `--mcp-config` (every
    /// [`Self::launch_for_open`] one does) passes through untouched, so
    /// the file is written once per session, not once per swap.
    fn with_mcp_config(&self, mut launch: SessionLaunch) -> Result<SessionLaunch, ProviderError> {
        if launch.argv.iter().any(|arg| arg == "--mcp-config") {
            return Ok(launch);
        }
        if let Some(path) = self.mcp_config_for(&launch.session_id.clone())? {
            launch.argv.push("--mcp-config".into());
            launch.argv.push(path);
            launch.argv.push("--strict-mcp-config".into());
        }
        Ok(launch)
    }

    /// Override `$HOME` for stored-history lookup (tests).
    pub fn with_home(mut self, home: PathBuf) -> Self {
        self.home_override = Some(home);
        self
    }

    /// Override the session workspace cwd for stored-history lookup
    /// (tests). Production sets this from `OpenSession.workspace` in
    /// [`Self::spawn_launch`]; it is what [`history::stored_transcript_path`]
    /// resolves and slugs.
    pub fn with_workspace(self, workspace: PathBuf) -> Self {
        *self.workspace.lock().expect("workspace mutex") = Some(workspace);
        self
    }

    fn home(&self) -> Option<PathBuf> {
        match &self.home_override {
            Some(home) => Some(home.clone()),
            None => std::env::var_os("HOME").map(PathBuf::from),
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
        let running = RunningChild::spawn(
            &self.program,
            launch,
            Arc::clone(&self.fold),
            self.tx.clone(),
        )
        .map_err(|error| ProviderError::Unavailable {
            reason: format!("could not spawn claude: {error}"),
        })?;
        *child = Some(running);
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
            let line = fold::decide_approval(&request, choice, feedback)?;
            Ok(ClaimedApproval::Known(request, line))
        } else {
            let request = fold.take_unknown(approval).expect("presence checked above");
            let line = fold::decide_unknown_approval(&request, choice, feedback)?;
            Ok(ClaimedApproval::Unknown(request, line))
        }
    }

    /// The relaunch a `SubmitInput` needs when its effort differs from the
    /// running child's launch flag: `--resume <session-id>` plus the new
    /// `--effort`, carrying the recorded `--model` along. `None` means the
    /// running child already flies this level and the turn goes straight to
    /// stdin. Pure — no spawn — so tests drive this without a CLI.
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
        Some(argv::argv_for_resume(session_id, model.as_deref(), None, wanted.as_deref()))
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
        let launch = self.with_mcp_config(launch.clone())?;
        let running = RunningChild::spawn(
            &self.program,
            &launch,
            Arc::clone(&self.fold),
            self.tx.clone(),
        )
        .map_err(|error| ProviderError::Unavailable {
            reason: format!("could not respawn claude: {error}"),
        })?;
        *self.child.lock().expect("child mutex") = Some(running);
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
        match self.child.lock().expect("child mutex").as_mut() {
            Some(running) => running.send_line(&line).map_err(|error| ProviderError::Unavailable {
                reason: format!("the session child is unreachable: {error}"),
            })?,
            None => {
                return Err(ProviderError::Unavailable {
                    reason: "no session child is running".into(),
                })
            }
        }
        // The ack carries the submission handle; the turn's true identity
        // arrives on the event stream (the child replays user messages).
        Ok(Ack::TurnAccepted { turn_id })
    }

    fn find_stored(&self, session_id: &str) -> Option<PathBuf> {
        // The doc §3 rule: resolve the session's own workspace cwd, slug
        // it, look in that one directory. An unresolvable cwd, an absent
        // slug directory, or a missing file all degrade to None — the
        // callers answer honest unavailable. Never scan other directories
        // when the workspace is known: that would serve one workspace's
        // transcript inside another.
        let home = self.home()?;
        let workspace = self.workspace.lock().expect("workspace mutex").clone();
        match workspace {
            Some(workspace) => history::stored_transcript_path(&workspace, session_id, Some(&home))
                .filter(|candidate| candidate.is_file()),
            // A fresh adapter after a restart holds no workspace (resume
            // carries no cwd): fall back to an exact-id scan over every
            // slug directory. The session id is a UUID and the match is
            // the exact `<id>.jsonl` filename — zero or several matches
            // still refuse — so this locates the session's own file
            // without ever serving a neighbor's.
            None => Self::scan_stored(&home, session_id),
        }
    }

    /// The stored transcript for `session_id` by exact filename, over
    /// every slug directory under `~/.claude/projects`. `Some` only on
    /// exactly one match: none — or two files claiming one id — refuses.
    fn scan_stored(home: &std::path::Path, session_id: &str) -> Option<PathBuf> {
        if session_id.is_empty() || session_id.contains('/') {
            return None;
        }
        let projects = home.join(".claude").join("projects");
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
        // parser tolerates that trailer.
        let output = std::process::Command::new(&self.program)
            .arg("--version")
            .output()
            .map_err(|error| ProviderError::Unavailable {
                reason: format!("could not ask claude its version: {error}"),
            })?;
        let raw = String::from_utf8_lossy(&output.stdout);
        let raw = raw.trim();
        let version = if raw.is_empty() {
            String::from_utf8_lossy(&output.stderr).trim().to_owned()
        } else {
            raw.to_owned()
        };
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
        match command {
            Command::OpenSession { request_id, workspace, model, .. } => {
                let launch =
                    self.launch_for_open(&request_id, workspace.as_deref(), model.as_deref())?;
                self.spawn_launch(&launch)
            }
            Command::ResumeSession { session_id, .. } => {
                let launch = self.launch_for_resume(&session_id)?;
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
            Command::ForkSession { request_id, session_id, .. } => {
                let launch = self.launch_for_fork(&request_id, &session_id)?;
                self.spawn_launch(&launch)
            }
            Command::ListSessions { workspace, .. } => {
                let home = self.home().ok_or_else(|| ProviderError::Unavailable {
                    reason: "no home directory to look for stored sessions under".into(),
                })?;
                let projects = home.join(".claude").join("projects");
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
                // Admission, like Codex: the running turn keeps its model
                // and the recorded one is the session's effective model
                // from here on — the chip, the next footers, and the next
                // resume's `--model`. The live child keeps its spawn-time
                // flag until that reopen; there is no per-turn model
                // channel over stream-json stdin, and spelling one would
                // be the lie this seam exists to prevent. Applies to a
                // session with turns in it exactly as to a fresh one:
                // nothing here counts turns.
                *self.model.lock().expect("model mutex") = Some(model.clone());
                self.fold.lock().expect("fold mutex").set_model(&model);
                Ok(Ack::Accepted)
            }
            Command::SelectApprovalMode { .. } => Err(ProviderError::unsupported(
                "select-approval-mode",
                "session config is spawn-time (--permission-mode); reopen the session to change it",
            )),
            Command::RunShell { .. } => Err(ProviderError::unsupported(
                "run-shell",
                "no out-of-turn shell surface was probed over stream-json stdin; the Bash tool \
                 runs inside turns",
            )),
            Command::SubmitInput { request_id, session_id, parts, effort, display_text, .. } => {
                // Effort is a launch flag: when the pick differs from the
                // running child's, the next turn relaunches with
                // `--resume <session-id> --effort <new>` before the text
                // goes to stdin. Same level — or none anywhere — skips the
                // swap and the turn goes straight in.
                if let Some(launch) = self.resume_launch_for_effort(&session_id, effort.as_deref()) {
                    self.check_session(&session_id)?;
                    self.relaunch(&launch)?;
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
            Command::ListModels { .. } => Err(ProviderError::unsupported(
                "list-models",
                "no model-catalog surface was probed; Baaz supplies the list",
            )),
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
    fn a_changed_effort_relaunches_with_resume_and_the_new_flag() {
        // Offline throughout: `resume_launch_for_effort` is pure — the
        // decision `SubmitInput` acts on — so no `claude` process spawns.
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        adapter.attach_session_for_tests("sess-9");
        // Same level (none anywhere): no swap, the turn goes straight in.
        assert!(
            adapter.resume_launch_for_effort("sess-9", None).is_none(),
            "no effort anywhere means no relaunch"
        );
        // A pick the child was not launched with: `--resume <id>` plus the
        // new `--effort`, carrying the recorded `--model` along.
        *adapter.model.lock().expect("model mutex") = Some("sonnet".into());
        let launch = adapter
            .resume_launch_for_effort("sess-9", Some("high"))
            .expect("a changed effort relaunches");
        let argv = &launch.argv;
        let resume = argv.iter().position(|arg| arg == "--resume").expect("--resume");
        assert_eq!(argv.get(resume + 1).map(String::as_str), Some("sess-9"));
        let effort = argv.iter().position(|arg| arg == "--effort").expect("--effort");
        assert_eq!(argv.get(effort + 1).map(String::as_str), Some("high"));
        let model = argv.iter().position(|arg| arg == "--model").expect("--model");
        assert_eq!(argv.get(model + 1).map(String::as_str), Some("sonnet"));
        assert_eq!(launch.session_id, "sess-9");
        // Once the child flies that level, the same pick is a no-op: the
        // turn must not pay a relaunch every send.
        *adapter.effort.lock().expect("effort mutex") = Some("high".into());
        assert!(
            adapter.resume_launch_for_effort("sess-9", Some("high")).is_none(),
            "the launched level needs no swap"
        );
        // And clearing back to Default relaunches without the flag.
        let launch = adapter
            .resume_launch_for_effort("sess-9", None)
            .expect("clearing the effort relaunches");
        assert!(
            !launch.argv.iter().any(|arg| arg == "--effort"),
            "default relaunches flagless: {:?}",
            launch.argv
        );
    }

    #[test]
    fn resume_seeding_marks_stored_user_echoes() {
        // Offline: plant a stored transcript under a fake home and seed
        // from it. The text echo's uuid marks; the tool-result frame (no
        // bubble) and the torn line mark nothing. Unknown workspace marks
        // nothing — honest unavailable, never a guess.
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
}
