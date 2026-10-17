//! Host control requests, confirmed by the child — not assumed.
//!
//! Every host→child control line (`initialize`, `set_model`,
//! `apply_flag_settings`) is tracked by its `request_id` from the send
//! until the child's `control_response` answers it. `success` confirms the
//! optimistic record; `error` — or no answer within
//! [`CONTROL_CONFIRM_TIMEOUT`] — rolls the recorded model/effort back,
//! banners the CLI's reason on the session, and (for effort) records a
//! resume relaunch so the level still lands. A reply that matches no
//! pending id changes nothing: the catalog, the model and the effort move
//! only on an id match.
//!
//! The hub owns exactly the state the pump thread needs to confirm on
//! arrival (the fold, the event sender, the pending map, the recorded
//! model/effort/session): the live pump and the tests share
//! [`ControlHub::ingest_line`], so the two paths cannot drift apart. The
//! hub never spawns — a due effort relaunch is recorded here and performed
//! by the adapter's `SubmitInput` arm, which already owns the spawn.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use aui_protocol::{Block, Delta, PermissionMode};
use crossbeam_channel::Sender;
use provider::ProviderEvent;

use crate::argv::{argv_for_resume, SessionLaunch};
use crate::child::StdinWriter;
use crate::fold::{step_line, ClaudeFold};

/// How long a host control request waits for the child's `control_response`
/// before the silence itself counts as a refusal. Ten seconds is generous
/// for a local child answering a flag flip, and short enough that a
/// rejected pick never poses as applied for long.
pub const CONTROL_CONFIRM_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a Read-only turn waits for the child to confirm the deny layer
/// on the owner's own MCP tools (the `mcp_status` answer, then the apply
/// carrying its denies) before going out. Past it the turn goes out only
/// over a layer the child already confirmed — at worst the
/// [`MCP_WILDCARD_DENY`] that rides until the first answer — and is
/// refused when there is none: failing closed, never open.
pub const MCP_LAYER_HOLD: Duration = Duration::from_secs(3);

/// The deny rule covering every MCP tool, whatever server serves it. The
/// CLI glob-matches deny rules on the tool name (2.1.276: the deny check
/// passes `globMatching: true`, and a rule with `*` becomes the regex
/// `^mcp__.*$`), so this denies the owner's configured servers, late
/// claude.ai connectors and dynamic ones alike. It is the Read-only
/// layer's MCP deny until the child's first `mcp_status` answer names
/// which tools may stay.
pub const MCP_WILDCARD_DENY: &str = "mcp__*";

/// The [`Block::Generic`] kind carrying a rejected model change: the
/// session banner shows its text, and the app resyncs the chip from the
/// adapter's catalog on sight. Only the Claude Code adapter emits this
/// kind; anything else carrying it is not ours to interpret.
pub const CONTROL_MODEL_REJECTED_CARD: &str = "claude-control-model-rejected";

/// The [`Block::Generic`] kind carrying a rejected approval-mode change:
/// the session banner shows its text. The chip needs no resync — it only
/// ever moves on the mode the child reports, and a refusal reports none.
pub const CONTROL_MODE_REJECTED_CARD: &str = "claude-control-mode-rejected";

/// The [`Block::Generic`] kind saying a model switch left Auto: the new
/// model has no auto mode, so the adapter asked the child into its asking
/// mode and the chip follows the mode the child then reports. The session
/// banner shows its text; nothing was refused.
pub const CONTROL_MODE_FALLBACK_CARD: &str = "claude-control-mode-fallback";

/// The [`Block::Generic`] kind carrying a rejected effort change: the
/// session banner shows its text. The chip needs no resync — the adapter
/// re-sends the picked level on the next submit, so chip and child
/// reconverge on the next turn.
pub const CONTROL_EFFORT_REJECTED_CARD: &str = "claude-control-effort-rejected";

/// What a pending host control request asked for: the join needs the kind
/// (only an `initialize` reply may move the catalog) and the state to
/// restore on refusal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PendingControlKind {
    /// The spawn handshake: success restates the catalog, error keeps the
    /// fallback and is logged.
    Initialize,
    /// A live model switch: error restores the previous model.
    SetModel,
    /// A live effort raise: error restores the previous level and records
    /// a resume relaunch carrying the wanted one. (A bogus `effortLevel`
    /// probed 2026-09-28 still answered `success`, so a refusal here means
    /// a genuine rejection or the timeout — never a spelling the CLI
    /// would have taken.)
    ApplyEffort,
    /// A live approval-mode switch (`set_permission_mode`, probed live
    /// 2026-10-16): success is followed by a `status` frame naming the
    /// mode, which is what moves the chip; error restores nothing (nothing
    /// moved) and banners the CLI's reason.
    SetPermissionMode(PermissionMode),
    /// The Read-only deny layer going on (the mode is Read-only) or off
    /// (the mode being left for) over `apply_flag_settings`: error
    /// banners and records a resume relaunch, whose argv carries the layer
    /// exactly when the mode the child then reports is Read-only — a
    /// refused layer never leaves Read-only quietly unenforced.
    ApplyReadOnly(PermissionMode),
    /// An `mcp_status` query (probed 2026-10-17): success reports the
    /// owner's MCP servers and their tools' annotations, from which the
    /// Read-only layer's MCP denies are refreshed; error or silence is
    /// logged and changes nothing — the next dispatch asks again.
    McpStatus,
}

/// One host→child control request awaiting its `control_response`.
#[derive(Clone, Debug)]
pub(crate) struct PendingControl {
    /// Which request this is: what may move on success, what restores on
    /// refusal.
    pub kind: PendingControlKind,
    /// The host-minted `request_id` the child's answer echoes.
    pub request_id: String,
    /// The model the `set_model` wanted, when this is one.
    pub wanted_model: Option<String>,
    /// The recorded model before the optimistic switch, when this is one.
    pub previous_model: Option<String>,
    /// The fold's model before the optimistic switch, when this is one.
    pub previous_fold_model: Option<String>,
    /// The effort level the `apply_flag_settings` wanted, when this is one.
    pub wanted_effort: Option<String>,
    /// The recorded effort before the optimistic raise, when this is one.
    pub previous_effort: Option<String>,
    /// The MCP denies a Read-only layer going on carries, when this is
    /// one: its success is what confirms them on the child.
    pub wanted_mcp: Option<Vec<String>>,
    /// When the line went out: silence past
    /// [`CONTROL_CONFIRM_TIMEOUT`] refuses, loudly.
    pub sent_at: Instant,
}

impl PendingControl {
    /// A request that records no model or effort to restore.
    pub(crate) fn bare(kind: PendingControlKind, request_id: String) -> Self {
        Self {
            kind,
            request_id,
            wanted_model: None,
            previous_model: None,
            previous_fold_model: None,
            wanted_effort: None,
            previous_effort: None,
            wanted_mcp: None,
            sent_at: Instant::now(),
        }
    }
}

/// Where the Read-only deny layer stands on the owner's own MCP tools.
///
/// Changed only under its own lock, and every layer line is queued (never
/// waited on) under that same lock — so the pump's refresh and a mode
/// pick's apply or clear are one step each, in one order on the wire: a
/// refresh can never land between a clear and the state saying "off".
#[derive(Debug, Default)]
pub(crate) struct McpLayer {
    /// Whether the running child holds the Read-only layer (`--settings`
    /// at launch, or applied live): only then do MCP denies go out.
    pub(crate) layer_on: bool,
    /// Whether the running child may serve the owner's own MCP servers
    /// (launched without `--strict-mcp-config`): only then does the layer
    /// need MCP denies before a turn, and only then does
    /// [`MCP_WILDCARD_DENY`] stand in for an unknown list.
    pub(crate) owner_mcp: bool,
    /// The denies the current child's last `mcp_status` answer called
    /// for, Read-only or not; `None` before any answer.
    pub(crate) known: Option<Vec<String>>,
    /// The layer apply on its way to the child: its `request_id` and the
    /// MCP denies it carries, until the child answers it.
    pub(crate) in_flight: Option<(String, Vec<String>)>,
    /// The MCP denies the child CONFIRMED on its layer (its `success`
    /// answer, or a `--settings` launch that needs none); `None` when no
    /// confirmed layer covers MCP tools.
    pub(crate) confirmed: Option<Vec<String>>,
}

impl McpLayer {
    /// The MCP denies the layer should carry now: the last answer's, or
    /// before any answer every MCP tool when the owner's servers may run.
    pub(crate) fn target(&self) -> Vec<String> {
        match &self.known {
            Some(known) => known.clone(),
            None if self.owner_mcp => vec![MCP_WILDCARD_DENY.to_owned()],
            None => Vec::new(),
        }
    }

    /// The denies the child holds or is about to: the apply in flight
    /// wins over the confirmed one.
    fn outstanding(&self) -> Option<&Vec<String>> {
        self.in_flight.as_ref().map(|(_, denies)| denies).or(self.confirmed.as_ref())
    }
}

/// The shared confirmation state: everything the pump thread needs to
/// confirm a `control_response` the moment it arrives. Held behind one
/// [`Arc`] by the adapter and cloned into each child pump — locks are held
/// one at a time, never nested, so the pump and a dispatch never deadlock.
pub struct ControlHub {
    /// The shared fold: outcomes queue here for confirmation.
    pub(crate) fold: Arc<Mutex<ClaudeFold>>,
    /// Where confirmations and refusal banners go.
    pub(crate) tx: Sender<ProviderEvent>,
    /// Host control requests awaiting their answer, by `request_id`.
    pub(crate) pending: Mutex<HashMap<String, PendingControl>>,
    /// The session's recorded model (see the adapter's `model`).
    pub(crate) model: Arc<Mutex<Option<String>>>,
    /// The effort level in effect (see the adapter's `effort`).
    pub(crate) effort: Arc<Mutex<Option<String>>>,
    /// The held session id, for scoping refusal banners.
    pub(crate) session_id: Arc<Mutex<Option<String>>>,
    /// A relaunch the next `SubmitInput` must perform: recorded when an
    /// `apply_flag_settings` refusal (effort, or the Read-only layer)
    /// arrives (the pump owns no spawn), consumed by the adapter arm that
    /// already owns the spawn.
    pub(crate) effort_relaunch_due: Mutex<Option<SessionLaunch>>,
    /// The model of the last `set_model` the child confirmed and no newer
    /// pick superseded: the adapter re-checks the approval mode against it
    /// on its next dispatch (the pump cannot write to the child), so a
    /// refused switch never moves the mode.
    pub(crate) confirmed_model: Mutex<Option<String>>,
    /// Which child's stream is the session's: each spawn takes the next
    /// epoch, and a deliberate hang-up (relaunch, close, quit) moves past
    /// its own. Only the current child's end of stream may close turns
    /// and report a lost connection — a relaunched child's predecessor
    /// ending must never tear down the child that replaced it.
    pub(crate) child_epoch: AtomicU64,
    /// Whether the current child's stream ended on its own: the next turn
    /// resumes the session instead of writing into a dead stdin. A child
    /// that never spawned was never lost.
    pub(crate) child_lost: AtomicBool,
    /// The current child's stdin writer, shared with its
    /// [`crate::child::RunningChild`] so the pump can queue the Read-only
    /// MCP refresh the moment an `mcp_status` answer lands — before the
    /// model can reach a tool — rather than on the next dispatch. Queued,
    /// never waited on: the pump never blocks on stdin. Tagged with the
    /// child's epoch.
    pub(crate) stdin: Mutex<Option<(u64, StdinWriter)>>,
    /// `mcp_status` and MCP-refresh `apply_flag_settings` lines the hub
    /// addressed, in order (`baaz-mcp-…` ids), recorded whether or not a
    /// child took them — what tests read.
    pub(crate) mcp_sent: Mutex<Vec<String>>,
    /// The `baaz-mcp-…` request id sequence.
    mcp_seq: AtomicU64,
    /// The Read-only layer's MCP state (see [`McpLayer`]).
    pub(crate) mcp: Mutex<McpLayer>,
    /// Signalled whenever [`Self::mcp`] may have settled (an answer, a
    /// refusal, a failed write, a lost child): what a held Read-only turn
    /// waits on.
    mcp_settled: Condvar,
}

impl ControlHub {
    /// Share confirmation state: the fold, the event sender and the
    /// recorded model/effort/session stay the adapter's — this hub holds
    /// the same [`Arc`]s, never copies.
    pub(crate) fn new(
        fold: Arc<Mutex<ClaudeFold>>,
        tx: Sender<ProviderEvent>,
        model: Arc<Mutex<Option<String>>>,
        effort: Arc<Mutex<Option<String>>>,
        session_id: Arc<Mutex<Option<String>>>,
    ) -> Self {
        Self {
            fold,
            tx,
            pending: Mutex::new(HashMap::new()),
            model,
            effort,
            session_id,
            effort_relaunch_due: Mutex::new(None),
            confirmed_model: Mutex::new(None),
            child_epoch: AtomicU64::new(0),
            child_lost: AtomicBool::new(false),
            stdin: Mutex::new(None),
            mcp_sent: Mutex::new(Vec::new()),
            mcp_seq: AtomicU64::new(0),
            mcp: Mutex::new(McpLayer::default()),
            mcp_settled: Condvar::new(),
        }
    }

    /// A child is starting: its stream becomes the session's. Returns the
    /// epoch its pump reports end of stream under.
    pub(crate) fn begin_child(&self) -> u64 {
        self.child_lost.store(false, Ordering::SeqCst);
        // A fresh child holds only the layer its argv gave it, and that
        // one carries no MCP denies; its servers are its own, so the last
        // child's answer no longer counts either.
        {
            let mut mcp = self.mcp.lock().expect("mcp mutex");
            mcp.layer_on = false;
            mcp.known = None;
            mcp.in_flight = None;
            mcp.confirmed = None;
        }
        self.child_epoch.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// The child of `epoch` writes through `stdin`: hub-initiated lines
    /// (the MCP refresh) go to the same writer its turns do.
    pub(crate) fn attach_stdin(&self, epoch: u64, stdin: StdinWriter) {
        *self.stdin.lock().expect("stdin mutex") = Some((epoch, stdin));
    }

    /// The child of `epoch` is gone: drop the hub's handle on its writer.
    /// A newer child's writer is left alone.
    pub(crate) fn detach_stdin(&self, epoch: u64) {
        let mut stdin = self.stdin.lock().expect("stdin mutex");
        if stdin.as_ref().is_some_and(|(attached, _)| *attached == epoch) {
            *stdin = None;
        }
    }

    /// The current child's writer, when one is attached. A retired
    /// child's writer is never handed out.
    fn current_writer(&self) -> Option<StdinWriter> {
        match &*self.stdin.lock().expect("stdin mutex") {
            Some((epoch, stdin)) if *epoch == self.child_epoch.load(Ordering::SeqCst) => Some(stdin.clone()),
            _ => None,
        }
    }

    /// Record, track and queue one hub-initiated control line (never
    /// waiting on the write — this runs on the pump). Returns its
    /// `request_id` and whether it was queued: a writer that has stopped
    /// untracks it at once (no answer can come). With no child attached
    /// the line is recorded and tracked but not delivered, as the
    /// adapter's own sends are.
    fn send_hub_control(
        &self,
        request: serde_json::Value,
        kind: PendingControlKind,
        wanted_mcp: Option<Vec<String>>,
    ) -> (String, bool) {
        let request_id = format!("baaz-mcp-{}", self.mcp_seq.fetch_add(1, Ordering::SeqCst));
        let line = serde_json::json!({
            "type": "control_request",
            "request_id": request_id,
            "request": request,
        })
        .to_string();
        self.mcp_sent.lock().expect("mcp sent mutex").push(line.clone());
        let mut pending = PendingControl::bare(kind, request_id.clone());
        pending.wanted_mcp = wanted_mcp;
        self.track(pending);
        let queued = match self.current_writer() {
            Some(writer) => writer.enqueue(line, Some(request_id.clone())).is_ok(),
            None => true,
        };
        if !queued {
            self.untrack(&request_id);
        }
        (request_id, queued)
    }

    /// Ask the child for its MCP servers (`mcp_status`), unless a query is
    /// already in flight. The answer refreshes the Read-only MCP denies.
    pub(crate) fn query_mcp_status(&self) {
        let in_flight = self
            .pending
            .lock()
            .expect("pending mutex")
            .values()
            .any(|pending| pending.kind == PendingControlKind::McpStatus);
        if in_flight {
            return;
        }
        self.send_hub_control(serde_json::json!({"subtype": "mcp_status"}), PendingControlKind::McpStatus, None);
        self.mcp_settled.notify_all();
    }

    /// A child was launched: `launched` says its argv carried the
    /// Read-only layer on `--settings`, `owner_mcp` that it may serve the
    /// owner's own MCP servers. A `--settings` layer carries no MCP
    /// denies, which is all a child that serves no owner servers needs —
    /// so only that one counts as confirmed; one that does gets the live
    /// layer from the adapter at once ([`Self::send_read_only_layer`]).
    pub(crate) fn note_launched_layer(&self, launched: bool, owner_mcp: bool) {
        let mut mcp = self.mcp.lock().expect("mcp mutex");
        mcp.layer_on = launched;
        mcp.owner_mcp = owner_mcp;
        mcp.in_flight = None;
        mcp.confirmed = (launched && !owner_mcp).then(Vec::new);
        drop(mcp);
        self.mcp_settled.notify_all();
    }

    /// Put the Read-only layer on live (`wanted` is Read-only, carrying
    /// [`McpLayer::target`]) or take it off (`wanted` is the mode being
    /// left for) as ONE step against the pump: the layer state moves and
    /// the line is queued under the [`McpLayer`] lock, so an `mcp_status`
    /// answer folding meanwhile either lands before (and is carried) or
    /// after (and sees the new state) — never between the write and the
    /// state change. Returns the line (for the adapter's record) and,
    /// when a child is attached, where its write result lands: the caller
    /// waits on it after this returns, never under the lock.
    pub(crate) fn send_read_only_layer(
        &self,
        request_id: String,
        wanted: PermissionMode,
    ) -> (String, Option<crossbeam_channel::Receiver<std::io::Result<()>>>) {
        let on = wanted == PermissionMode::DenyUnmatched;
        let mut mcp = self.mcp.lock().expect("mcp mutex");
        let denies = if on { mcp.target() } else { Vec::new() };
        let line = crate::apply_read_only_control_request(&request_id, on, &denies);
        let mut pending = PendingControl::bare(PendingControlKind::ApplyReadOnly(wanted), request_id.clone());
        pending.wanted_mcp = on.then(|| denies.clone());
        self.track(pending);
        let reply = self.current_writer().map(|writer| writer.enqueue_with_reply(line.clone()));
        mcp.layer_on = on;
        mcp.confirmed = None;
        mcp.in_flight = on.then_some((request_id, denies));
        drop(mcp);
        self.mcp_settled.notify_all();
        (line, reply)
    }

    /// The layer line `request_id` never reached the child: untrack it and
    /// put the state back to what the child still holds — no layer when
    /// it was going on, the layer when it was coming off.
    pub(crate) fn read_only_layer_unsent(&self, request_id: &str, on: bool) {
        self.untrack(request_id);
        let mut mcp = self.mcp.lock().expect("mcp mutex");
        if mcp.in_flight.as_ref().is_some_and(|(id, _)| id == request_id) {
            mcp.in_flight = None;
        }
        mcp.layer_on = !on;
        drop(mcp);
        self.mcp_settled.notify_all();
    }

    /// An `mcp_status` answer landed: remember the denies it calls for and,
    /// while the Read-only layer is on and they differ from what the child
    /// holds or is about to, apply the layer again carrying them
    /// (`apply_flag_settings` replaces the layer's `permissions`, so the
    /// base rules ride along). Decided and queued under the [`McpLayer`]
    /// lock (see [`Self::send_read_only_layer`]); the denies count as held
    /// only once the child answers `success`, so a write that fails — or a
    /// refusal — leaves the next identical answer free to send again. A
    /// refusal falls back like any refused layer: banner plus a relaunch
    /// carrying it.
    fn refresh_mcp_denies(&self, servers: &[crate::argv::McpServerStatus]) {
        let denies = crate::argv::read_only_mcp_denies(servers);
        let unsent = {
            let mut mcp = self.mcp.lock().expect("mcp mutex");
            mcp.known = Some(denies.clone());
            if !mcp.layer_on || mcp.outstanding() == Some(&denies) {
                None
            } else {
                let (request_id, queued) = self.send_hub_control(
                    serde_json::json!({
                        "subtype": "apply_flag_settings",
                        "settings": crate::argv::read_only_settings_with(&denies),
                    }),
                    PendingControlKind::ApplyReadOnly(PermissionMode::DenyUnmatched),
                    Some(denies.clone()),
                );
                if queued {
                    mcp.in_flight = Some((request_id, denies));
                    None
                } else {
                    Some(request_id)
                }
            }
        };
        if let Some(request_id) = unsent {
            self.refuse(
                PendingControl::bare(PendingControlKind::ApplyReadOnly(PermissionMode::DenyUnmatched), request_id),
                "the child's stdin writer has stopped",
            );
        }
    }

    /// A hub-initiated line for the child of `epoch` failed to write: its
    /// request can never be answered, so refuse it now (a refused layer
    /// banners and owes a relaunch carrying it) rather than at its
    /// timeout. A retired child's failures change nothing.
    pub(crate) fn hub_write_failed(&self, epoch: u64, request_id: &str, reason: &str) {
        if self.child_epoch.load(Ordering::SeqCst) != epoch {
            return;
        }
        let pending = self.pending.lock().expect("pending mutex").remove(request_id);
        if let Some(pending) = pending {
            self.refuse(pending, &format!("could not write to the child: {reason}"));
        }
        self.mcp_settled.notify_all();
    }

    /// Hold a Read-only turn until the child confirmed the layer's MCP
    /// denies: no `mcp_status` query open, no layer apply in flight, and
    /// the confirmed denies the ones the last answer called for. Returns
    /// at once when there is nothing to hold for — the layer is off, the
    /// child serves no owner servers, or no child is attached to deliver
    /// to. Past `timeout`, a layer the child confirmed earlier (at worst
    /// [`MCP_WILDCARD_DENY`]) still lets the turn go; none refuses it,
    /// failing closed.
    pub(crate) fn await_mcp_layer(&self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        let attached = self.current_writer().is_some();
        let mut mcp = self.mcp.lock().expect("mcp mutex");
        loop {
            if !mcp.layer_on || !mcp.owner_mcp || !attached {
                return Ok(());
            }
            if self.child_lost.load(Ordering::SeqCst) {
                return Err("the Claude Code process exited before it confirmed the Read-only rules".into());
            }
            let asking = self
                .pending
                .lock()
                .expect("pending mutex")
                .values()
                .any(|pending| pending.kind == PendingControlKind::McpStatus);
            if !asking && mcp.in_flight.is_none() && mcp.confirmed.as_ref() == Some(&mcp.target()) {
                return Ok(());
            }
            let now = Instant::now();
            if now >= deadline {
                return match mcp.confirmed {
                    Some(_) => Ok(()),
                    None => Err(format!(
                        "Claude Code has not confirmed the Read-only rules for your MCP tools within {}s; \
                         the turn was not sent",
                        timeout.as_secs()
                    )),
                };
            }
            mcp = self.mcp_settled.wait_timeout(mcp, deadline - now).expect("mcp mutex").0;
        }
    }

    /// The child of `epoch` is being hung up on purpose: its end of stream
    /// is expected, not a loss. A newer child's epoch is left alone.
    pub(crate) fn retire_child(&self, epoch: u64) {
        let _ = self.child_epoch.compare_exchange(epoch, epoch + 1, Ordering::SeqCst, Ordering::SeqCst);
    }

    /// Track one host control request from its send until its answer.
    pub(crate) fn track(&self, pending: PendingControl) {
        self.pending.lock().expect("pending mutex").insert(pending.request_id.clone(), pending);
    }

    /// Forget one tracked request: the send never reached the child, so no
    /// answer can arrive and no timeout may fire for it.
    pub(crate) fn untrack(&self, request_id: &str) {
        self.pending.lock().expect("pending mutex").remove(request_id);
    }

    /// Forget every tracked request: the child they were sent to is gone
    /// (a relaunch killed it), so no answer can arrive, and a timeout firing
    /// later would refuse against the NEW child's state — rolling back a
    /// pick it legitimately holds and bannering a stale failure.
    pub(crate) fn forget_pending(&self) {
        self.pending.lock().expect("pending mutex").clear();
        self.mcp.lock().expect("mcp mutex").in_flight = None;
        self.mcp_settled.notify_all();
    }

    /// Take a due effort relaunch, when a refusal recorded one. The take
    /// is the claim: at most one `SubmitInput` performs it.
    pub(crate) fn take_effort_relaunch(&self) -> Option<SessionLaunch> {
        self.effort_relaunch_due.lock().expect("relaunch mutex").take()
    }

    /// Take the model of a confirmed `set_model`, when one landed since the
    /// last take. The take is the claim.
    pub(crate) fn take_confirmed_model(&self) -> Option<String> {
        self.confirmed_model.lock().expect("confirmed model mutex").take()
    }

    /// Fold one raw child stdout line and confirm whatever it settles: the
    /// one function the live pump and the tests share.
    pub fn ingest_line(&self, line: &str) {
        {
            let mut fold = self.fold.lock().expect("fold mutex");
            step_line(&mut fold, line, &mut |event| {
                let _ = self.tx.send(event);
            });
        }
        self.drain_confirmations();
    }

    /// [`Self::ingest_line`] for one line of the child of `epoch`: a
    /// retired or replaced child's late lines — a status or init frame
    /// naming its old permission mode, a stale delta — never reach the
    /// fold, exactly as its end of stream does not.
    pub fn ingest_line_of(&self, epoch: u64, line: &str) {
        if self.child_epoch.load(Ordering::SeqCst) != epoch {
            return;
        }
        self.ingest_line(line);
    }

    /// The current child's stdout ended (`epoch` is the one
    /// [`Self::begin_child`] gave it; a retired or replaced child's end is
    /// ignored): whatever turn it was running can never finish, so close it with a [`crate::CHILD_EXITED_CARD`] naming
    /// `reason` (see [`ClaudeFold::abandon_live_turns`]), forget every
    /// control request it will never answer, then report the lost
    /// connection. The live pump calls this once, after its last line —
    /// the same function the closed-stream tests drive.
    pub fn ingest_end_of_stream(&self, epoch: u64, reason: &str) {
        if self.child_epoch.load(Ordering::SeqCst) != epoch {
            return;
        }
        if self.child_lost.swap(true, Ordering::SeqCst) {
            return;
        }
        let (session_id, deltas) = {
            let mut fold = self.fold.lock().expect("fold mutex");
            let session_id = fold.session_id().map(str::to_owned);
            (session_id, fold.abandon_live_turns(reason))
        };
        self.forget_pending();
        if !deltas.is_empty() {
            let _ = self.tx.send(ProviderEvent::Deltas { session_id, deltas });
        }
        let _ = self.tx.send(ProviderEvent::ConnectionLost { reason: reason.to_owned() });
        self.mcp_settled.notify_all();
    }

    /// Confirm every queued answer, then refuse every request the child
    /// left unanswered past [`CONTROL_CONFIRM_TIMEOUT`]. Runs after each
    /// folded line and at each dispatch, so a refusal lands even when the
    /// child answers nothing more.
    pub fn drain_confirmations(&self) {
        let outcomes = self.fold.lock().expect("fold mutex").drain_control_outcomes();
        for outcome in outcomes {
            self.confirm_outcome(
                &outcome.request_id,
                &outcome.subtype,
                outcome.error.as_deref(),
                outcome.models,
                outcome.mcp_servers,
            );
        }
        self.expire_timeouts();
        self.mcp_settled.notify_all();
    }

    /// Confirm one answer against its pending request: `success` finalises
    /// the optimistic record (an `initialize` reply restates the catalog),
    /// `error` rolls back and banners. A foreign id — or a subtype that is
    /// neither — changes nothing: a non-answer stays pending until its
    /// timeout, a stray never moves state.
    fn confirm_outcome(
        &self,
        request_id: &str,
        subtype: &str,
        error: Option<&str>,
        models: Vec<crate::frame::CatalogModel>,
        mcp_servers: Option<Vec<crate::argv::McpServerStatus>>,
    ) {
        let Some(pending) = self.pending.lock().expect("pending mutex").remove(request_id) else {
            return;
        };
        match subtype {
            "success" => match pending.kind {
                PendingControlKind::Initialize => self.fold.lock().expect("fold mutex").set_catalog(models),
                PendingControlKind::SetModel => {
                    // Owned read first (see `refuse`): only the pick still
                    // on record counts — a superseded one answers for
                    // nothing.
                    let current = self.model.lock().expect("model mutex").clone();
                    if current.is_some() && current == pending.wanted_model {
                        *self.confirmed_model.lock().expect("confirmed model mutex") = current;
                    }
                }
                PendingControlKind::McpStatus => {
                    if let Some(servers) = mcp_servers {
                        self.refresh_mcp_denies(&servers);
                    }
                }
                PendingControlKind::ApplyReadOnly(PermissionMode::DenyUnmatched) => {
                    // The child holds these denies now — the only moment
                    // they count as held.
                    let mut mcp = self.mcp.lock().expect("mcp mutex");
                    if mcp.in_flight.as_ref().is_some_and(|(id, _)| *id == pending.request_id) {
                        mcp.in_flight = None;
                        mcp.confirmed = pending.wanted_mcp.clone();
                    }
                }
                _ => {}
            },
            "error" => {
                self.refuse(pending, &error_reason(error));
            }
            _ => {
                // Neither confirmation nor refusal: the child said
                // something else under our id, so the request stays
                // pending and its timeout still guards it.
                self.pending
                    .lock()
                    .expect("pending mutex")
                    .insert(pending.request_id.clone(), pending);
            }
        }
    }

    /// Refuse every request unanswered past [`CONTROL_CONFIRM_TIMEOUT`]:
    /// silence is a refusal with a clear message, handled exactly like a
    /// CLI `error`.
    fn expire_timeouts(&self) {
        let now = Instant::now();
        let stale: Vec<PendingControl> = {
            let mut pending = self.pending.lock().expect("pending mutex");
            let ids: Vec<String> = pending
                .iter()
                .filter(|(_, request)| now.duration_since(request.sent_at) > CONTROL_CONFIRM_TIMEOUT)
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter().filter_map(|id| pending.remove(&id)).collect()
        };
        for pending in stale {
            self.refuse(pending, &format!("no answer arrived within {}s", CONTROL_CONFIRM_TIMEOUT.as_secs()));
        }
    }

    /// Refuse one tracked request: roll back what its optimistic send
    /// recorded, and surface the reason. A request superseded by a newer
    /// pick (the recorded value already moved on) drops silently — the
    /// newer request owns the state and answers for itself.
    fn refuse(&self, pending: PendingControl, reason: &str) {
        match pending.kind {
            PendingControlKind::Initialize => {
                eprintln!(
                    "[provider-claude-code] initialize {} refused ({}); keeping the fallback catalog",
                    pending.request_id, reason
                );
            }
            PendingControlKind::McpStatus => {
                eprintln!(
                    "[provider-claude-code] mcp_status {} refused ({}); the next dispatch asks again",
                    pending.request_id, reason
                );
            }
            PendingControlKind::SetModel => {
                let wanted = pending.wanted_model.clone().unwrap_or_default();
                // Owned reads first: a `match`/`if` scrutinee would hold
                // the guard through the body, deadlocking the re-lock
                // below (`std` mutexes are not reentrant).
                let current = self.model.lock().expect("model mutex").clone();
                if current.as_deref() != pending.wanted_model.as_deref() {
                    // A newer pick already moved the record: it owns the
                    // state now, so this stale refusal restores nothing.
                    return;
                }
                *self.model.lock().expect("model mutex") = pending.previous_model.clone();
                {
                    let mut fold = self.fold.lock().expect("fold mutex");
                    match pending.previous_fold_model.as_deref() {
                        Some(previous) => fold.set_model(previous),
                        None => fold.clear_model(),
                    }
                }
                let actual = pending.previous_model.clone().unwrap_or_else(|| "(no model recorded)".into());
                self.emit_card(
                    CONTROL_MODEL_REJECTED_CARD,
                    format!(
                        "Claude Code rejected the model change to \"{wanted}\" ({reason}); still on \"{actual}\"."
                    ),
                );
            }
            PendingControlKind::SetPermissionMode(wanted) => {
                self.emit_card(
                    CONTROL_MODE_REJECTED_CARD,
                    format!(
                        "Claude Code rejected the approval mode change to \"{}\" ({reason}).",
                        wanted.label()
                    ),
                );
            }
            PendingControlKind::ApplyReadOnly(wanted) => {
                // No answer will confirm it: the next `mcp_status` answer
                // may send the same denies again.
                {
                    let mut mcp = self.mcp.lock().expect("mcp mutex");
                    if mcp.in_flight.as_ref().is_some_and(|(id, _)| *id == pending.request_id) {
                        mcp.in_flight = None;
                    }
                }
                let session_id = self.session_id.lock().expect("session mutex").clone();
                let what = if wanted == PermissionMode::DenyUnmatched { "add" } else { "remove" };
                match session_id {
                    Some(session_id) => {
                        let model = self.model.lock().expect("model mutex").clone();
                        let effort = self.effort.lock().expect("effort mutex").clone();
                        // No mode on the launch: the adapter's relaunch
                        // fills in the mode the child last reported, and
                        // the deny layer follows that mode.
                        let launch = argv_for_resume(&session_id, model.as_deref(), None, effort.as_deref());
                        *self.effort_relaunch_due.lock().expect("relaunch mutex") = Some(launch);
                        self.emit_card(
                            CONTROL_MODE_REJECTED_CARD,
                            format!(
                                "Claude Code refused to {what} the Read-only rules ({reason}); falling back to a resume relaunch."
                            ),
                        );
                    }
                    None => {
                        self.emit_card(
                            CONTROL_MODE_REJECTED_CARD,
                            format!("Claude Code refused to {what} the Read-only rules ({reason})."),
                        );
                    }
                }
            }
            PendingControlKind::ApplyEffort => {
                let wanted = pending.wanted_effort.clone().unwrap_or_default();
                // Owned reads first (see above): the session guard must be
                // released before `emit_card` locks it again.
                if *self.effort.lock().expect("effort mutex") != pending.wanted_effort {
                    // A newer level already moved the record: it owns the
                    // state now, so this stale refusal restores nothing.
                    return;
                }
                *self.effort.lock().expect("effort mutex") = pending.previous_effort.clone();
                let session_id = self.session_id.lock().expect("session mutex").clone();
                match session_id {
                    Some(session_id) => {
                        let model = self.model.lock().expect("model mutex").clone();
                        let launch =
                            argv_for_resume(&session_id, model.as_deref(), None, Some(&wanted));
                        *self.effort_relaunch_due.lock().expect("relaunch mutex") = Some(launch);
                        self.emit_card(
                            CONTROL_EFFORT_REJECTED_CARD,
                            format!(
                                "Claude Code rejected the effort change to \"{wanted}\" ({reason}); falling back to a resume relaunch."
                            ),
                        );
                    }
                    None => {
                        self.emit_card(
                            CONTROL_EFFORT_REJECTED_CARD,
                            format!(
                                "Claude Code rejected the effort change to \"{wanted}\" ({reason}); no session is held, so the level stays off."
                            ),
                        );
                    }
                }
            }
        }
    }

    /// Scope one refusal banner to the held session and send it down the
    /// event stream. The card's kind tells the app what failed; its text is
    /// the sentence the banner shows.
    pub(crate) fn emit_card(&self, kind: &str, text: String) {
        let session_id = self.session_id.lock().expect("session mutex").clone();
        let _ = self.tx.send(ProviderEvent::Deltas {
            session_id,
            deltas: vec![Delta::BlockAdded {
                turn_id: "control-error".into(),
                block: Block::Generic { kind: kind.into(), status: "error".into(), text },
            }],
        });
    }
}

/// The refusal reason to banner: the CLI's text when it named one, a plain
/// fallback when it refused bare. The fallback lives at the confirmation
/// site — the decoder reports `None`, never a guess.
fn error_reason(error: Option<&str>) -> String {
    error
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| "the child refused without naming a reason".into())
}
