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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aui_protocol::{Block, Delta, PermissionMode};
use crossbeam_channel::Sender;
use provider::ProviderEvent;

use crate::argv::{argv_for_resume, SessionLaunch};
use crate::fold::{step_line, ClaudeFold};

/// How long a host control request waits for the child's `control_response`
/// before the silence itself counts as a refusal. Ten seconds is generous
/// for a local child answering a flag flip, and short enough that a
/// rejected pick never poses as applied for long.
pub const CONTROL_CONFIRM_TIMEOUT: Duration = Duration::from_secs(10);

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
    /// When the line went out: silence past
    /// [`CONTROL_CONFIRM_TIMEOUT`] refuses, loudly.
    pub sent_at: Instant,
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
        }
    }

    /// A child is starting: its stream becomes the session's. Returns the
    /// epoch its pump reports end of stream under.
    pub(crate) fn begin_child(&self) -> u64 {
        self.child_lost.store(false, Ordering::SeqCst);
        self.child_epoch.fetch_add(1, Ordering::SeqCst) + 1
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
    }

    /// Confirm every queued answer, then refuse every request the child
    /// left unanswered past [`CONTROL_CONFIRM_TIMEOUT`]. Runs after each
    /// folded line and at each dispatch, so a refusal lands even when the
    /// child answers nothing more.
    pub fn drain_confirmations(&self) {
        let outcomes = self.fold.lock().expect("fold mutex").drain_control_outcomes();
        for outcome in outcomes {
            self.confirm_outcome(&outcome.request_id, &outcome.subtype, outcome.error.as_deref(), outcome.models);
        }
        self.expire_timeouts();
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
