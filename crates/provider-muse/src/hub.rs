//! One `muse serve` child, many sessions: the hub and its lanes.
//!
//! Claude Code and Codex run one child per session, so a session's
//! [`Provider`] owns its child. Muse multiplexes every session over one
//! child, so the child belongs to a [`MuseHub`] and each session gets a
//! [`MuseLane`]: a [`ProviderAdapter`] whose commands go to the shared child
//! and whose event stream carries only its own session.
//!
//! The reader thread folds every event into the hub's one [`MuseFold`] and
//! routes what it produced — deltas, the facts beside them, the taps on the
//! shoulder — to the lane bound to that session, all under one lock, so the
//! order a lane sees is the order the fold folded in. Events for a session
//! no lane is bound to yet wait (bounded) and are handed over when one
//! binds, so the events a `session/start` streams before its answer are not
//! lost. Row-level facts (a turn running or settling, a status change) also
//! go to the connection stream, which the application reads for its rows.
//!
//! What only Muse needs stays here, behind the neutral vocabulary: the
//! approval stage token is filled from the fold, an approval someone else
//! settled is resolved in the fold, a mode pick moves the sandbox profile
//! with it, a resumed session's profile is matched to its mode, transcript
//! pages fold in order with the live stream, and a dead child is respawned
//! with every followed session resumed from its cursor.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{unbounded, Receiver, Sender};
use muse_adapter::MuseFold;
use muse_client::schema::{
    ApprovalDecideParams, ApprovalListPendingParams, ClientCapabilities, ModelListParams, ModelSelection,
    SessionForkParams, SessionResumeParams, SessionSetModelParams, ViewPageParams,
};
use muse_client::{new_command_id, MuseClient, MuseError, MuseEvent};
use provider::facts::SessionSide;
use provider::{
    Ack, CapabilitySet, Command, ConnectInfo, Handshake, PendingApproval, PendingQuestion,
    ProviderAdapter, ProviderError, ProviderEvent, ProviderId,
};

use crate::facts::FactTracker;
use crate::terminal::TerminalRelay;
use crate::translate::{self, FoldAccess};

/// How many events wait for a session no lane is bound to, across all such
/// sessions. Oldest go first: a session nobody opens has nothing to lose.
const ORPHAN_LIMIT: usize = 512;

/// The connection a hub currently rides.
#[derive(Clone)]
struct Link {
    client: Arc<MuseClient>,
    handshake: Handshake,
    user_shell: bool,
    session_mcp: bool,
    warning: Option<String>,
}

/// Where events go: the fold, the facts last sent, the bound lanes, and the
/// events waiting for a lane. One lock, so fold order is delivery order.
struct Routed {
    fold: MuseFold,
    tracker: FactTracker,
    routes: HashMap<String, Sender<ProviderEvent>>,
    /// Lanes that see every session's events (tests, and the transitional
    /// whole-connection adapter).
    watchers: Vec<Sender<ProviderEvent>>,
    orphans: VecDeque<(String, ProviderEvent)>,
}

/// What a model pick needs from the last catalog row: profile, provider,
/// display label.
type CatalogRow = (Option<String>, String, String);

struct Core {
    /// What to spawn on a reconnect; `None` for a hub built on a client the
    /// caller spawned (tests), which cannot respawn.
    program: Option<String>,
    info: Mutex<Option<ConnectInfo>>,
    link: Mutex<Option<Link>>,
    /// The client before `initialize`, for a hub built on a spawned client.
    pending: Mutex<Option<Arc<MuseClient>>>,
    routed: Mutex<Routed>,
    connection: (Sender<ProviderEvent>, Receiver<ProviderEvent>),
    terminal: Mutex<Option<TerminalRelay>>,
    /// Bumped per child: a reader of an older child stops quietly.
    generation: AtomicU64,
    /// muse refused the terminal bridge on a resume once; later resumes go
    /// without it rather than paying the refused round trip.
    refuses_resume_bridge: AtomicBool,
    /// Model id → (profile, provider) from the last catalog, for a model
    /// pick's selection.
    catalog: Mutex<HashMap<String, CatalogRow>>,
    /// One mode pick at a time per session: two picks interleaving could
    /// land the mode of one and the profile of the other.
    picks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// The last usage reading seen (a `usage/changed`, a `usage/read`):
    /// what [`MuseLane::read_usage`] peeks at.
    usage: Mutex<Option<provider::UsageReport>>,
}

impl FoldAccess for Core {
    fn with_fold(&self, f: &mut dyn FnMut(&mut MuseFold)) {
        f(&mut self.routed.lock().expect("routed mutex").fold);
    }
}

/// The one `muse serve` child and the sessions riding it.
#[derive(Clone)]
pub struct MuseHub {
    core: Arc<Core>,
}

impl MuseHub {
    fn with(program: Option<String>, pending: Option<Arc<MuseClient>>) -> Self {
        Self {
            core: Arc::new(Core {
                program,
                info: Mutex::new(None),
                link: Mutex::new(None),
                pending: Mutex::new(pending),
                routed: Mutex::new(Routed {
                    fold: MuseFold::new(),
                    tracker: FactTracker::new(),
                    routes: HashMap::new(),
                    watchers: Vec::new(),
                    orphans: VecDeque::new(),
                }),
                connection: unbounded(),
                terminal: Mutex::new(None),
                generation: AtomicU64::new(0),
                refuses_resume_bridge: AtomicBool::new(false),
                catalog: Mutex::new(HashMap::new()),
                picks: Mutex::new(HashMap::new()),
                usage: Mutex::new(None),
            }),
        }
    }

    /// Spawn `muse serve` ([`crate::spawn`]) and shake hands. Blocking.
    pub fn connect(program: &str, info: &ConnectInfo) -> Result<Self, ProviderError> {
        let hub = Self::with(Some(program.to_owned()), None);
        hub.attach(crate::spawn(program)?, info)?;
        Ok(hub)
    }

    /// A hub over a child the caller already spawned, not yet initialized:
    /// the first lane's [`ProviderAdapter::connect`] shakes hands. Such a hub
    /// cannot respawn.
    pub fn from_client(client: MuseClient) -> Self {
        Self::with(None, Some(Arc::new(client)))
    }

    /// `initialize` over `client` and start its reader.
    fn attach(&self, client: MuseClient, info: &ConnectInfo) -> Result<Handshake, ProviderError> {
        self.attach_shared(Arc::new(client), info)
    }

    fn attach_shared(&self, client: Arc<MuseClient>, info: &ConnectInfo) -> Result<Handshake, ProviderError> {
        let (result, warning) = client
            .initialize(
                &info.client_name,
                &info.client_version,
                ClientCapabilities {
                    // `account/*` is experimental: without the opt-in every
                    // account method answers `experimentalRequired`.
                    experimental_api: Some(true),
                    opt_out_notification_methods: None,
                    requested_capabilities: (!info.capabilities.is_empty()).then(|| info.capabilities.clone()),
                    ..Default::default()
                },
            )
            .map_err(translate::transport_error)?;
        let handshake = Handshake {
            provider: ProviderId::Muse,
            agent_name: result.server_info.name.clone(),
            agent_version: result.server_info.version.clone(),
        };
        let granted = |name: &str| result.granted_capabilities.iter().any(|c| c.as_wire() == Some(name));
        let link = Link {
            client: Arc::clone(&client),
            handshake: handshake.clone(),
            user_shell: granted("userShell"),
            session_mcp: granted("sessionMcp"),
            warning: warning.map(|warning| format!("{warning:?}")),
        };
        *self.core.info.lock().expect("info mutex") = Some(info.clone());
        *self.core.link.lock().expect("link mutex") = Some(link);
        let generation = self.core.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let core = Arc::clone(&self.core);
        let events = client.events();
        std::thread::Builder::new()
            .name("muse-hub-reader".into())
            .spawn(move || {
                while let Ok(event) = events.recv() {
                    if core.generation.load(Ordering::SeqCst) != generation {
                        return;
                    }
                    if !core.forward(event) {
                        return;
                    }
                }
            })
            .map_err(|error| ProviderError::Unavailable { reason: format!("could not start the reader: {error}") })?;
        Ok(handshake)
    }

    /// A new lane: unbound until an open, resume or fork names its session.
    pub fn lane(&self) -> MuseLane {
        let (tx, rx) = unbounded();
        MuseLane { core: Arc::clone(&self.core), session: Mutex::new(None), tx, rx, watch_all: false, connected: false }
    }

    /// A lane that sees every session's events: for tests and the
    /// transitional whole-connection adapter.
    pub fn watch_all(&self) -> MuseLane {
        let mut lane = self.lane();
        lane.watch_all = true;
        self.core.routed.lock().expect("routed mutex").watchers.push(lane.tx.clone());
        lane
    }

    /// Row-level facts for every session (a turn running or settling, a
    /// status change) and the connection's own loss and return. Competing
    /// consumers: keep one.
    pub fn connection_events(&self) -> Receiver<ProviderEvent> {
        self.core.connection.1.clone()
    }

    /// The current child, shared — transitional: the application's
    /// session list still reads Muse's rows over it until they come
    /// through the seam.
    pub fn transport(&self) -> Option<Arc<MuseClient>> {
        self.core.link.lock().expect("link mutex").as_ref().map(|link| Arc::clone(&link.client))
    }

    /// Who answered the handshake.
    pub fn handshake(&self) -> Option<Handshake> {
        self.core.link.lock().expect("link mutex").as_ref().map(|link| link.handshake.clone())
    }

    /// Whether `initialize` granted `userShell`.
    pub fn user_shell_granted(&self) -> bool {
        self.core.link.lock().expect("link mutex").as_ref().is_some_and(|link| link.user_shell)
    }

    /// Whether `initialize` granted `sessionMcp` (muse ≥ 1.3): the terminal
    /// relay's route.
    pub fn session_mcp_granted(&self) -> bool {
        self.core.link.lock().expect("link mutex").as_ref().is_some_and(|link| link.session_mcp)
    }

    /// The schema-fingerprint warning from `initialize`, for the log.
    pub fn schema_warning(&self) -> Option<String> {
        self.core.link.lock().expect("link mutex").as_ref().and_then(|link| link.warning.clone())
    }

    /// What the child last said on stderr, for the log after it died.
    pub fn stderr_tail(&self) -> String {
        self.transport().map(|client| client.stderr_tail()).unwrap_or_default()
    }

    /// Point the terminal relay at the bridge and socket. It rides only on
    /// the `sessionMcp` grant.
    pub fn set_terminal_relay(&self, bridge: std::path::PathBuf, socket: std::path::PathBuf) {
        *self.core.terminal.lock().expect("terminal mutex") = Some(TerminalRelay { bridge, socket });
    }

    /// Whether muse refused the terminal bridge on a resume this run.
    pub fn refuses_resume_bridge(&self) -> bool {
        self.core.refuses_resume_bridge.load(Ordering::Relaxed)
    }

    /// The sessions lanes are bound to.
    pub fn followed(&self) -> Vec<String> {
        self.core.routed.lock().expect("routed mutex").routes.keys().cloned().collect()
    }

    /// Respawn the child and re-attach every followed session from its
    /// cursor (`session/resume` serving only the suffix). Returns each
    /// session whose resume was refused, with the refusal, for the caller to
    /// show; the rest stream on as before. Blocking.
    pub fn reconnect(&self) -> Result<Vec<(String, ProviderError)>, ProviderError> {
        let Some(program) = self.core.program.clone() else {
            return Err(ProviderError::Unavailable { reason: "this connection cannot respawn".into() });
        };
        let info = self.core.info.lock().expect("info mutex").clone().unwrap_or_else(|| ConnectInfo::new("baaz", "0"));
        self.attach(crate::spawn(&program)?, &info)?;
        let mut refused = Vec::new();
        for session_id in self.followed() {
            let cursor = {
                let routed = self.core.routed.lock().expect("routed mutex");
                routed.fold.side(&session_id).map(|side| side.last_cursor.clone()).filter(|cursor| !cursor.is_empty())
            };
            if let Err(error) = self.core.resume(new_command_id(), &session_id, cursor, true) {
                refused.push((session_id, error));
            }
        }
        self.core.broadcast(ProviderEvent::ConnectionRestored);
        Ok(refused)
    }

    /// Hang up the child. Every lane hears it once, now: the reader of the
    /// child is retired first, so its own exit notice never follows.
    pub fn shutdown(&self) {
        self.core.generation.fetch_add(1, Ordering::SeqCst);
        if self.core.link.lock().expect("link mutex").is_some() {
            self.core.broadcast(ProviderEvent::ConnectionLost { reason: "the agent process exited".into() });
        }
        if let Some(link) = self.core.link.lock().expect("link mutex").take() {
            if let Ok(mut client) = Arc::try_unwrap(link.client) {
                client.shutdown();
            }
        }
    }
}

impl MuseHub {
    /// The current child, or why there is none.
    pub(crate) fn client(&self) -> Result<Arc<MuseClient>, ProviderError> {
        self.core.client()
    }

    /// Route `session_id`'s events to `tx` until the returned guard drops:
    /// a lane of the hub's own, for work no app lane follows.
    pub(crate) fn bind_private(&self, session_id: &str, tx: Sender<ProviderEvent>) -> PrivateBinding {
        self.core.bind(session_id, &tx);
        PrivateBinding { core: Arc::clone(&self.core), session_id: session_id.to_owned(), tx }
    }
}

/// A private binding ([`MuseHub::bind_private`]); unbinds on drop.
pub(crate) struct PrivateBinding {
    core: Arc<Core>,
    session_id: String,
    tx: Sender<ProviderEvent>,
}

impl Drop for PrivateBinding {
    fn drop(&mut self) {
        self.core.unbind(&self.session_id, &self.tx);
    }
}

impl Core {
    fn client(&self) -> Result<Arc<MuseClient>, ProviderError> {
        self.link
            .lock()
            .expect("link mutex")
            .as_ref()
            .map(|link| Arc::clone(&link.client))
            .ok_or_else(|| ProviderError::Unavailable { reason: "Muse is not running".into() })
    }

    /// Send `event` to every lane and the connection stream.
    fn broadcast(&self, event: ProviderEvent) {
        let routed = self.routed.lock().expect("routed mutex");
        for tx in routed.routes.values().chain(routed.watchers.iter()) {
            let _ = tx.send(event.clone());
        }
        let _ = self.connection.0.send(event);
    }

    /// Fold one transport event and route what it produced. `false` once
    /// the child is gone.
    fn forward(&self, event: MuseEvent) -> bool {
        match event {
            MuseEvent::Closed(code) => {
                let reason = match code {
                    Some(code) => format!("the agent process exited ({code})"),
                    None => "the agent process exited".into(),
                };
                self.broadcast(ProviderEvent::ConnectionLost { reason });
                false
            }
            MuseEvent::Notification { session_id: None, method, params, .. }
                if crate::usage::connection_event(&method, &params).is_some() =>
            {
                // Account and usage belong to the connection: the app's
                // sign-in and tier follow them; no session folds them.
                if let Some(event) = crate::usage::connection_event(&method, &params) {
                    if let ProviderEvent::UsageChanged { report } = &event {
                        *self.usage.lock().expect("usage mutex") = Some(report.clone());
                    }
                    let _ = self.connection.0.send(event);
                }
                true
            }
            MuseEvent::Notification { session_id, method, params, cursor } => {
                let mut routed = self.routed.lock().expect("routed mutex");
                // A wire fault belongs to the connection, not to a session:
                // every followed session shows it.
                let targets: Vec<Option<String>> = match &session_id {
                    None if method == muse_client::PROTOCOL_ERROR => {
                        routed.routes.keys().cloned().map(Some).collect()
                    }
                    other => vec![other.clone()],
                };
                for target in targets {
                    let deltas = routed.fold.apply(MuseEvent::Notification {
                        method: method.clone(),
                        params: params.clone(),
                        cursor: cursor.clone(),
                        session_id: target.clone(),
                    });
                    // `session/started` files its id inside `session`.
                    let Some(session) = target.or_else(|| routed.fold.last_touched().map(str::to_owned)) else {
                        continue;
                    };
                    let mut out = Vec::new();
                    if !deltas.is_empty() {
                        out.push(ProviderEvent::Deltas { session_id: Some(session.clone()), deltas });
                    }
                    let Routed { fold, tracker, .. } = &mut *routed;
                    out.extend(tracker.after(fold, &session, &method, &params));
                    route(&mut routed, &self.connection.0, &session, out, true);
                }
                true
            }
            MuseEvent::ServerRequest { method, params, id } => {
                let tap = match crate::tap_for(&method, &params) {
                    Ok(tap) => tap,
                    Err(error) => Some(ProviderEvent::ConnectionLost { reason: error.to_string() }),
                };
                let session = params.get("sessionId").and_then(serde_json::Value::as_str).map(str::to_owned);
                let mut routed = self.routed.lock().expect("routed mutex");
                let deltas = routed.fold.apply(MuseEvent::ServerRequest { id, method, params });
                let Some(session) = session else { return true };
                let mut out = Vec::new();
                if !deltas.is_empty() {
                    out.push(ProviderEvent::Deltas { session_id: Some(session.clone()), deltas });
                }
                out.extend(tap);
                let Routed { fold, tracker, .. } = &mut *routed;
                out.extend(tracker.changed(fold, &session));
                route(&mut routed, &self.connection.0, &session, out, true);
                true
            }
        }
    }

    /// Bind `session_id` to a lane: route its events there from now on, keep
    /// it folded, hand over what waited for it, then its facts whole.
    fn bind(&self, session_id: &str, tx: &Sender<ProviderEvent>) {
        let mut routed = self.routed.lock().expect("routed mutex");
        routed.fold.pin(session_id);
        routed.routes.insert(session_id.to_owned(), tx.clone());
        let waiting: Vec<(String, ProviderEvent)> = std::mem::take(&mut routed.orphans).into_iter().collect();
        for (session, event) in waiting {
            if session == session_id {
                let _ = tx.send(event);
            } else {
                routed.orphans.push_back((session, event));
            }
        }
        let Routed { fold, tracker, .. } = &mut *routed;
        tracker.forget(session_id);
        for fact in tracker.changed(fold, session_id) {
            let _ = tx.send(fact);
        }
    }

    /// Stop routing `session_id` to `tx` (another lane may have taken it).
    fn unbind(&self, session_id: &str, tx: &Sender<ProviderEvent>) {
        let mut routed = self.routed.lock().expect("routed mutex");
        if routed.routes.get(session_id).is_some_and(|bound| bound.same_channel(tx)) {
            routed.routes.remove(session_id);
            routed.fold.unpin(session_id);
        }
    }

    /// Route events produced off the reader thread (a page, a pending pull,
    /// a resolution), under the fold lock so they order with the live ones.
    fn route_produced(&self, session_id: &str, make: impl FnOnce(&mut MuseFold) -> Vec<provider::Delta>) {
        self.route_produced_then(session_id, make, || None);
    }

    /// [`Self::route_produced`], with `last` after everything it produced.
    fn route_produced_then(
        &self,
        session_id: &str,
        make: impl FnOnce(&mut MuseFold) -> Vec<provider::Delta>,
        last: impl FnOnce() -> Option<ProviderEvent>,
    ) {
        let mut routed = self.routed.lock().expect("routed mutex");
        let deltas = make(&mut routed.fold);
        let mut out = Vec::new();
        if !deltas.is_empty() {
            out.push(ProviderEvent::Deltas { session_id: Some(session_id.to_owned()), deltas });
        }
        let Routed { fold, tracker, .. } = &mut *routed;
        out.extend(tracker.changed(fold, session_id));
        out.extend(last());
        route(&mut routed, &self.connection.0, session_id, out, false);
    }

    /// The terminal bridge config for `session_id`, when there is a route.
    fn terminal_config(&self, session_id: &str) -> Option<muse_client::schema::SessionConfig> {
        let granted = self.link.lock().expect("link mutex").as_ref().is_some_and(|link| link.session_mcp);
        if !granted {
            return None;
        }
        self.terminal.lock().expect("terminal mutex").as_ref().map(|relay| relay.session_config(session_id))
    }

    /// `session/resume` without items (history pages separately), with the
    /// terminal bridge when it rides — retried bare when muse refuses the
    /// bridge — then the sandbox profile matched to the stored mode.
    fn resume(&self, request_id: String, session_id: &str, cursor: Option<String>, metadata_only: bool) -> Result<Ack, ProviderError> {
        let client = self.client()?;
        let bare = SessionResumeParams {
            command_id: request_id,
            session_id: session_id.to_owned(),
            cursor,
            exclude_items: metadata_only.then_some(true),
            ..Default::default()
        };
        let bridged = match self.terminal_config(session_id) {
            Some(config) if !self.refuses_resume_bridge.load(Ordering::Relaxed) => {
                SessionResumeParams { config: Some(config), ..bare.clone() }
            }
            _ => bare.clone(),
        };
        let result = match client.session_resume(&bridged) {
            Err(error) if bridged != bare && error.is_session_mcp_refused() => {
                self.refuses_resume_bridge.store(true, Ordering::Relaxed);
                client.session_resume(&bare)
            }
            other => other,
        }
        .map_err(translate::transport_error)?;
        // The stored session, folded as `session/started` would carry it:
        // the header, the mode, a fork's provenance.
        self.fold_envelope(session_id, &result.session);
        if let Err(text) = crate::modes::reconcile(&client, &result.session) {
            self.notice(session_id, text);
        }
        Ok(Ack::Session { session_id: result.session.session_id, title: result.session.title })
    }

    /// Fold a session object as the `session/started` that would carry it,
    /// and route what it moved.
    fn fold_envelope(&self, session_id: &str, session: &muse_client::schema::Session) {
        let Ok(envelope) = serde_json::to_value(session) else { return };
        self.route_produced(session_id, |fold| {
            fold.apply(MuseEvent::Notification {
                method: "session/started".into(),
                params: serde_json::json!({ "session": envelope }),
                cursor: None,
                session_id: Some(session_id.to_owned()),
            })
        });
    }

    /// Put `text` on the session's banner.
    fn notice(&self, session_id: &str, text: String) {
        let mut routed = self.routed.lock().expect("routed mutex");
        route(&mut routed, &self.connection.0, session_id, vec![ProviderEvent::Notice { session_id: session_id.to_owned(), text }], false);
    }

    /// The pick lock for `session_id`.
    fn pick_lock(&self, session_id: &str) -> Arc<Mutex<()>> {
        Arc::clone(self.picks.lock().expect("picks mutex").entry(session_id.to_owned()).or_default())
    }

    fn dispatch(&self, lane: &MuseLane, command: Command) -> Result<Ack, ProviderError> {
        match command {
            Command::OpenSession { request_id, workspace, model, model_provider, mode } => {
                // The session's id is minted here, from the request id
                // (`session/start` takes an exact identity), and the lane
                // binds before the start: nothing the start streams before
                // its answer is lost.
                let session_id = request_id.clone();
                lane.bind(&session_id);
                let client = self.client()?;
                let started = client
                    .session_start(&muse_client::schema::SessionStartParams {
                        // No mode, no pick: the host default stands.
                        permission_profile: mode.map(|mode| muse_adapter::permission_profile(mode).to_owned()),
                        approval_mode: mode.map(crate::modes::wire_mode),
                        command_id: request_id,
                        config: self.terminal_config(&session_id),
                        model_id: model,
                        provider_id: model_provider,
                        session_id: Some(session_id.clone()),
                        workspace_root: workspace,
                        workspace_roots: None,
                    })
                    .map_err(translate::transport_error)?;
                // The answer carries the session `session/started` would: the
                // only report of a mode set at start (no `approvalModeChanged`
                // follows one).
                // The id is ours: an answer under another one is a session
                // this lane never asked for, and nothing keyed by the minted
                // id could follow it.
                if started.session.session_id != session_id {
                    return Err(ProviderError::Rejected {
                        reason: format!(
                            "the session started under another id ({}) than the one asked for ({session_id})",
                            started.session.session_id
                        ),
                    });
                }
                self.fold_envelope(&session_id, &started.session);
                Ok(Ack::Session { session_id, title: started.session.title })
            }
            // Muse histories always page in (`PageTranscript`), so a resume
            // only attaches: the answer's items would go unread.
            Command::ResumeSession { request_id, session_id, cursor, .. } => {
                lane.bind(&session_id);
                self.resume(request_id, &session_id, cursor, true)
            }
            Command::ForkSession { request_id, session_id, through_turn, mode, .. } => {
                let client = self.client()?;
                let forked = client
                    .session_fork(&SessionForkParams {
                        command_id: request_id,
                        cut_point: through_turn.map(|last_turn_id| muse_client::schema::ForkCutPoint { last_turn_id }),
                        // The fork's history pages in once its lane resumes it,
                        // like every Muse attach: the answer's items go unread.
                        exclude_items: Some(true),
                        session_id: session_id.clone(),
                    })
                    .map_err(translate::transport_error)?;
                // The fork keeps the parent's mode when it came back in
                // another: its profile follows the pick.
                if let (Some(mode), Some(held)) = (mode, crate::modes::session_mode(&forked.session)) {
                    if held != mode {
                        if let Err(error) = crate::modes::pick(&client, None, &forked.session.session_id, held, mode) {
                            self.notice(&session_id, error.to_string());
                        }
                    }
                }
                Ok(Ack::Session { session_id: forked.session.session_id, title: forked.session.title })
            }
            Command::SelectApprovalMode { request_id, session_id, mode } => {
                let lock = self.pick_lock(&session_id);
                let _one_at_a_time = lock.lock().expect("pick mutex");
                let from = self
                    .routed
                    .lock()
                    .expect("routed mutex")
                    .fold
                    .session(&session_id)
                    .map(|session| session.mode)
                    .unwrap_or(aui_protocol::PermissionMode::OnRequest);
                crate::modes::pick(&*self.client()?, Some(&request_id), &session_id, from, mode)?;
                Ok(Ack::Accepted)
            }
            Command::ListModels { session } => {
                let client = self.client()?;
                let result = client
                    .model_list(&ModelListParams { session_id: session })
                    .map_err(translate::transport_error)?;
                let mut catalog = self.catalog.lock().expect("catalog mutex");
                for row in &result.models {
                    catalog.insert(
                        row.model_id.clone(),
                        (row.profile_id.clone(), row.provider_id.clone(), row.display_label.clone()),
                    );
                }
                Ok(Ack::ModelCatalog { models: translate::model_summaries(&result.models), provider: result.provider_id })
            }
            Command::SelectModel { request_id, session_id, model, model_provider } => {
                let row = self.catalog.lock().expect("catalog mutex").get(&model).cloned();
                self.client()?
                    .session_set_model(&SessionSetModelParams {
                        command_id: request_id,
                        model: ModelSelection {
                            display_label: row.as_ref().map(|(_, _, label)| label.clone()),
                            model_id: model,
                            profile_id: row.as_ref().and_then(|(profile, _, _)| profile.clone()),
                            provider_id: model_provider.or(row.map(|(_, provider, _)| provider)),
                        },
                        session_id,
                    })
                    .map_err(translate::transport_error)?;
                Ok(Ack::Accepted)
            }
            Command::DecideApproval { request_id, session_id, approval, choice, stage_token, feedback } => {
                // The stage token is the fold's to know: a decision sent
                // without one takes the approval's current stage.
                let requirement = match stage_token.as_deref().and_then(muse_adapter::parse_stage_token) {
                    Some(requirement) => Some(requirement),
                    None => self
                        .routed
                        .lock()
                        .expect("routed mutex")
                        .fold
                        .side(&session_id)
                        .and_then(|side| side.pending_approvals.get(&approval))
                        .map(|request| request.current_requirement_id.clone()),
                };
                let Some(requirement_id) = requirement else {
                    return Err(ProviderError::unknown_approval(&approval, "nothing pending under that id"));
                };
                let decided = self.client()?.approval_decide(&ApprovalDecideParams {
                    approval_id: approval.clone(),
                    choice_id: choice,
                    command_id: request_id,
                    feedback,
                    requirement_id,
                    session_id: session_id.clone(),
                });
                match decided {
                    Ok(_) => Ok(Ack::Accepted),
                    Err(error) => {
                        self.settle_lost_decision(&session_id, &approval, &error);
                        Err(translate::transport_error(error))
                    }
                }
            }
            Command::ListPending { session_id } => {
                let client = self.client()?;
                let pending = client
                    .approval_list_pending(&ApprovalListPendingParams { session_id: session_id.clone() })
                    .map_err(translate::transport_error)?;
                // The server never re-issues a request it already sent, so a
                // client that was away pulls; the fold dedupes what it saw.
                self.route_produced(&session_id, |fold| {
                    let mut deltas = Vec::new();
                    let mut refold = |method: &str, value: serde_json::Value| {
                        deltas.extend(fold.apply(MuseEvent::Notification {
                            method: method.to_owned(),
                            cursor: value.get("viewCursor").and_then(|v| v.as_str()).map(str::to_owned),
                            params: value,
                            session_id: Some(session_id.clone()),
                        }));
                    };
                    for approval in &pending.approvals {
                        if let Ok(value) = serde_json::to_value(approval) {
                            refold("approval/requested", value);
                        }
                    }
                    for request in &pending.user_inputs {
                        if let Ok(value) = serde_json::to_value(request) {
                            refold("userInput/requested", value);
                        }
                    }
                    deltas
                });
                Ok(Ack::PendingWork {
                    approvals: pending
                        .approvals
                        .iter()
                        .map(|approval| PendingApproval {
                            id: approval.approval_id.clone(),
                            session_id: approval.session_id.clone(),
                            headline: crate::approval_headline(approval),
                            stage_token: Some(muse_adapter::stage_token(&approval.current_requirement_id)),
                        })
                        .collect(),
                    questions: pending
                        .user_inputs
                        .iter()
                        .map(|prompt| PendingQuestion {
                            id: prompt.user_input_id.clone(),
                            session_id: prompt.session_id.clone(),
                            headline: crate::question_headline(prompt),
                        })
                        .collect(),
                })
            }
            Command::PageTranscript { session_id, after, limit, backward } => {
                let page = self
                    .client()?
                    .view_page(&ViewPageParams {
                        anchor: None,
                        cursor: after,
                        direction: translate::page_direction(backward),
                        limit,
                        session_id: session_id.clone(),
                    })
                    .map_err(translate::transport_error)?;
                // The page's deltas travel the lane in fold order with the
                // live stream; the ack only says where the next page starts
                // (none once a page comes back empty).
                let next_cursor = if page.events.is_empty() { None } else { page.next_cursor.clone() };
                let failed = std::cell::RefCell::new(None);
                self.route_produced_then(
                    &session_id,
                    |fold| match translate::fold_page(fold, page) {
                        Ok((deltas, _)) => deltas,
                        Err(error) => {
                            *failed.borrow_mut() = Some(error);
                            Vec::new()
                        }
                    },
                    // After the page's deltas, in the same stream: what the
                    // caller pages on, so it never reads a page as drawn
                    // before it is.
                    || {
                        failed.borrow().is_none().then(|| ProviderEvent::TranscriptPaged {
                            session_id: session_id.clone(),
                            next_cursor: next_cursor.clone(),
                        })
                    },
                );
                let failed = failed.into_inner();
                if let Some(error) = failed {
                    return Err(error);
                }
                Ok(Ack::TranscriptPage { deltas: Vec::new(), next_cursor })
            }
            // The whole stored output, when no length is asked for: paged
            // in, decoded, capped at [`crate::output::FULL_OUTPUT_CAP`]; the
            // ack's `complete` is false when the cap cut it.
            Command::ReadStoredOutput { session_id, item, output, offset: 0, length: None } => {
                let (content, complete) = crate::output::read_whole(&*self.client()?, &session_id, &item, &output)?;
                Ok(Ack::StoredOutput { content, complete })
            }
            Command::ReadUsage => {
                let ack = translate::dispatch(&*self.client()?, self, Command::ReadUsage, None)?;
                if let Ack::Usage { report: Some(report) } = &ack {
                    *self.usage.lock().expect("usage mutex") = Some(report.clone());
                }
                Ok(ack)
            }
            other => {
                let session = command_session(&other);
                let client = self.client()?;
                let ack = translate::dispatch(&client, self, other, None)?;
                // A submission may have queued, a steer recorded its words:
                // the facts beside the transcript moved.
                if let Some(session) = session {
                    let mut routed = self.routed.lock().expect("routed mutex");
                    let Routed { fold, tracker, .. } = &mut *routed;
                    let facts = tracker.changed(fold, &session);
                    route(&mut routed, &self.connection.0, &session, facts, false);
                }
                Ok(ack)
            }
        }
    }

    /// An `approval/decide` that lost: someone else settled it (the error
    /// carries the winning resolution) or the server forgot it. Either way
    /// the card on screen is a lie and no `approval/resolved` will follow,
    /// so the fold settles it here.
    fn settle_lost_decision(&self, session_id: &str, approval: &str, error: &MuseError) {
        use muse_client::schema::ErrorKind;
        let resolution = match error.kind() {
            Some(ErrorKind::ApprovalAlreadyResolved) => match error {
                MuseError::Rpc(rpc) => rpc.data.as_ref().and_then(|data| data.resolution.clone()),
                _ => None,
            },
            Some(ErrorKind::ApprovalNotFound) => None,
            _ => return,
        };
        self.route_produced(session_id, |fold| fold.resolve_approval(session_id, approval, resolution));
    }
}

/// Deliver `events` for `session`: to its lane when one is bound (and to
/// every watcher), else into the bounded waiting room. Row-level facts from
/// the live stream also go to the connection stream.
fn route(routed: &mut Routed, connection: &Sender<ProviderEvent>, session: &str, events: Vec<ProviderEvent>, live: bool) {
    for event in events {
        if live
            && matches!(
                event,
                ProviderEvent::TurnRunning { .. } | ProviderEvent::TurnSettled { .. } | ProviderEvent::StatusChanged { .. }
            )
        {
            let _ = connection.send(event.clone());
        }
        for watcher in &routed.watchers {
            let _ = watcher.send(event.clone());
        }
        match routed.routes.get(session) {
            Some(tx) => {
                let _ = tx.send(event);
            }
            None => {
                if routed.orphans.len() >= ORPHAN_LIMIT {
                    routed.orphans.pop_front();
                }
                routed.orphans.push_back((session.to_owned(), event));
            }
        }
    }
}

/// The session a command acts on, when it names one.
fn command_session(command: &Command) -> Option<String> {
    match command {
        Command::SubmitInput { session_id, .. }
        | Command::SteerInput { session_id, .. }
        | Command::InterruptTurn { session_id, .. }
        | Command::CancelTurn { session_id, .. }
        | Command::ReclaimQueued { session_id, .. }
        | Command::CompactSession { session_id, .. }
        | Command::RunShell { session_id, .. } => Some(session_id.clone()),
        _ => None,
    }
}

/// One session riding a [`MuseHub`]: commands go to the shared child, and
/// the event stream carries only this session once it is bound.
pub struct MuseLane {
    core: Arc<Core>,
    session: Mutex<Option<String>>,
    tx: Sender<ProviderEvent>,
    rx: Receiver<ProviderEvent>,
    watch_all: bool,
    connected: bool,
}

impl MuseLane {
    fn bind(&self, session_id: &str) {
        if self.watch_all {
            // A watcher already sees every session: binding a route too
            // would deliver each event twice.
            self.core.routed.lock().expect("routed mutex").fold.pin(session_id);
            *self.session.lock().expect("session mutex") = Some(session_id.to_owned());
            return;
        }
        let mut bound = self.session.lock().expect("session mutex");
        if bound.as_deref() == Some(session_id) {
            return;
        }
        if let Some(previous) = bound.replace(session_id.to_owned()) {
            self.core.unbind(&previous, &self.tx);
        }
        drop(bound);
        self.core.bind(session_id, &self.tx);
    }

    /// The session this lane follows, once an open, resume or fork bound it.
    pub fn session(&self) -> Option<String> {
        self.session.lock().expect("session mutex").clone()
    }

    /// The hub this lane rides.
    pub fn hub(&self) -> MuseHub {
        MuseHub { core: Arc::clone(&self.core) }
    }
}

impl ProviderAdapter for MuseLane {
    fn id(&self) -> ProviderId {
        ProviderId::Muse
    }

    /// The hub already shook hands; a lane only reports who answered. On a
    /// hub built over a client the caller spawned, the first lane's connect
    /// is the handshake.
    fn connect(&mut self, client: &ConnectInfo) -> Result<Handshake, ProviderError> {
        if self.connected {
            return Err(ProviderError::Rejected { reason: "already connected".into() });
        }
        let pending = self.core.pending.lock().expect("pending mutex").take();
        let handshake = match pending {
            Some(transport) => MuseHub { core: Arc::clone(&self.core) }.attach_shared(transport, client)?,
            None => MuseHub { core: Arc::clone(&self.core) }
                .handshake()
                .ok_or_else(|| ProviderError::Unavailable { reason: "Muse is not running".into() })?,
        };
        self.connected = true;
        Ok(handshake)
    }

    fn capabilities(&self) -> CapabilitySet {
        let link = self.core.link.lock().expect("link mutex").clone();
        let version = link.as_ref().map(|link| link.handshake.agent_version.clone());
        crate::caps::capabilities_for_link(
            version.as_deref().unwrap_or(crate::caps::MUSE_VERSION_FLOOR),
            link.as_ref().map(|link| link.user_shell),
        )
    }

    fn dispatch(&self, command: Command) -> Result<Ack, ProviderError> {
        self.core.dispatch(self, command)
    }

    fn events(&self) -> Receiver<ProviderEvent> {
        self.rx.clone()
    }

    /// Stop following: the session is unbound and may be evicted. The
    /// shared child stays — other sessions ride it.
    fn shutdown(&mut self) {
        if let Some(session) = self.session.lock().expect("session mutex").take() {
            self.core.unbind(&session, &self.tx);
        }
        if self.watch_all {
            self.core.routed.lock().expect("routed mutex").watchers.retain(|tx| !tx.same_channel(&self.tx));
        }
    }

    fn read_usage(&self) -> Option<provider::UsageReport> {
        self.core.usage.lock().expect("usage mutex").clone()
    }
}

impl Drop for MuseLane {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The side state a test reads: what the hub would send a newly bound lane.
#[doc(hidden)]
pub fn side_of(hub: &MuseHub, session_id: &str) -> Option<SessionSide> {
    let routed = hub.core.routed.lock().expect("routed mutex");
    routed.fold.side(session_id).map(muse_adapter::neutral_side)
}
