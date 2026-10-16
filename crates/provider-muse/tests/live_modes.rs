//! The approval-mode matrix against a real `muse serve`: four chip modes ×
//! {set at open, switched mid-session}, one real turn per cell (two for a
//! mid-session cell). Driven the way Baaz's muse lane drives it — the
//! legacy transport: `session/start` carrying `approvalMode`,
//! `session/setApprovalMode` for the switch, `approval/decide` echoing the
//! current requirement — with the chip read from
//! `session/approvalModeChanged`, exactly what the fold reads.
//!
//! **Spends real turns, and this Muse login is pay-as-you-go.**
//! `#[ignore]`d and gated on `BAAZ_LIVE_MODES=1`; pick the cheapest model
//! (`BAAZ_LIVE_MODES_LIST=1` prints the catalog without spending):
//!
//! ```text
//! BAAZ_LIVE_MODES=1 BAAZ_LIVE_MODES_MODEL=<model> BAAZ_LIVE_MODES_PROVIDER=<provider> \
//!   cargo test -p provider-muse --test live_modes -- --ignored --nocapture
//! ```

#[path = "../../provider/tests/support/live_modes.rs"]
mod live_modes;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use aui_protocol::PermissionMode;
use muse_client::schema::{
    ApprovalDecideParams, ApprovalListPendingParams, ApprovalMode, ClientCapabilities,
    SessionResumeParams, SessionSetApprovalModeParams, SessionStartParams, TurnInputPart, TurnStartParams,
};
use muse_client::{new_command_id, MuseClient, MuseConfig, MuseEvent};

use live_modes::{probe_prompt, tail, Cell, Outcome, Paths, WARMUP};

/// Whether each mode also selects its permission profile, the way Baaz's
/// muse lane does (`muse_adapter::permission_profile`). `BAAZ_LIVE_MODES_MUSE_PROFILES=0`
/// drives approval modes alone, for comparison.
fn profiles() -> bool {
    std::env::var("BAAZ_LIVE_MODES_MUSE_PROFILES").as_deref() != Ok("0")
}

const TURN_TIMEOUT: Duration = Duration::from_secs(240);

fn wire(mode: PermissionMode) -> ApprovalMode {
    match mode {
        PermissionMode::AllowAll => ApprovalMode::AllowAll,
        PermissionMode::OnRequest => ApprovalMode::OnRequest,
        PermissionMode::PromptUnmatched => ApprovalMode::PromptUnmatched,
        PermissionMode::DenyUnmatched => ApprovalMode::DenyUnmatched,
    }
}

/// Spawned the way `provider_muse::spawn` spawns: durable, trusted
/// workspace, the desktop-agent env scrubbed.
fn spawn() -> MuseClient {
    let program = muse_client::resolve_muse_program()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "muse".into());
    let removals = provider::child_env::scrubbed_removals();
    let client = MuseClient::spawn_with_env(
        &MuseConfig { program: program.into(), trust_workspace: true, no_session_log: false, extra_args: Vec::new() },
        &removals,
        &[],
    )
    .expect("muse serve starts");
    client
        .initialize("baaz", env!("CARGO_PKG_VERSION"), ClientCapabilities::default())
        .expect("initialize");
    client
}

struct Lane {
    client: MuseClient,
    events: crossbeam_channel::Receiver<MuseEvent>,
    session_id: String,
    reported: Option<PermissionMode>,
    decided: HashSet<(String, String)>,
}

#[derive(Default)]
struct Seen {
    asked: Vec<String>,
    answered: Vec<String>,
    text: String,
    finished: bool,
    /// Approvals the host raised and settled before a person could answer.
    auto_resolved: Vec<String>,
}

impl Lane {
    fn fold(&mut self, event: MuseEvent, seen: &mut Seen, turn_id: &str, deny_next: &mut bool) {
        match event {
            MuseEvent::Notification { method, params, .. } => {
                if params.get("sessionId").and_then(|v| v.as_str()) != Some(self.session_id.as_str()) {
                    return;
                }
                match method.as_str() {
                    "session/approvalModeChanged" => {
                        if let Some(mode) = params
                            .get("mode")
                            .and_then(|m| serde_json::from_value::<ApprovalMode>(m.clone()).ok())
                        {
                            self.reported = Some(muse_adapter::permission_mode(&mode));
                        }
                    }
                    "turn/completed" => {
                        if params.get("turnId").and_then(|v| v.as_str()) == Some(turn_id) {
                            seen.finished = true;
                        }
                    }
                    "item/delta" => {
                        if let Some(text) = params.pointer("/delta/text").and_then(|v| v.as_str()) {
                            seen.text.push_str(text);
                        }
                    }
                    "approval/requested" | "approval/updated" => self.decide_pending(seen, deny_next),
                    _ => {}
                }
            }
            MuseEvent::ServerRequest { method, .. } if method == "approval/request" => {
                self.decide_pending(seen, deny_next)
            }
            MuseEvent::ServerRequest { method, .. } => seen.asked.push(format!("SERVER REQUEST {method}")),
            MuseEvent::Closed(code) => {
                seen.answered.push(format!("muse exited {code:?}"));
                seen.finished = true;
            }
        }
    }

    /// Decide every pending requirement not decided yet: the first with a
    /// denial when `deny_next`, the rest with the once-scoped approval.
    fn decide_pending(&mut self, seen: &mut Seen, deny_next: &mut bool) {
        let Ok(pending) = self
            .client
            .approval_list_pending(&ApprovalListPendingParams { session_id: self.session_id.clone() })
        else {
            return;
        };
        for request in pending.approvals {
            let key = (request.approval_id.clone(), format!("{:?}", request.current_requirement_id));
            if !self.decided.insert(key) {
                continue;
            }
            let subject = request.subject.command.clone().or(request.subject.path.clone()).unwrap_or_default();
            seen.asked.push(format!("{} {} {}", request.tool_name, request.subject.kind, subject));
            let deny = std::mem::take(deny_next);
            let choice = request.available_choices.iter().find(|choice| {
                let label = format!("{:?}", choice.decision);
                // Muse offers a denial as `denied` or, for a shell stage,
                // only `abort` ("Reject") — the card's one refusal.
                if deny { label == "Denied" || label == "Abort" } else { label == "Approved" }
            });
            let Some(choice) = choice else {
                seen.answered.push(format!("no {} choice in {:?}", if deny { "deny" } else { "allow" }, request.available_choices));
                continue;
            };
            let result = self.client.approval_decide(&ApprovalDecideParams {
                approval_id: request.approval_id.clone(),
                choice_id: choice.choice_id.clone(),
                command_id: new_command_id(),
                feedback: None,
                requirement_id: request.current_requirement_id.clone(),
                session_id: self.session_id.clone(),
            });
            match result {
                Ok(_) => seen.answered.push((if deny { "deny" } else { "allow" }).to_owned()),
                // The host settled it itself (Read-only's deny, between
                // `approval/requested` and the pending read): it never
                // waited on a person, so it is not an ask.
                Err(error) if error.to_string().contains("already resolved") => {
                    let headline = seen.asked.pop().unwrap_or_default();
                    seen.auto_resolved.push(headline);
                }
                Err(error) => seen.answered.push(format!("decide ERR {error}")),
            }
        }
    }

    fn settle(&mut self, ms: u64) {
        let until = Instant::now() + Duration::from_millis(ms);
        let mut scratch = Seen::default();
        let mut no = false;
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            match self.events.recv_timeout(left) {
                Ok(event) => self.fold(event, &mut scratch, "", &mut no),
                Err(_) => break,
            }
        }
    }

    fn turn(&mut self, text: &str, deny_first: bool) -> Result<Seen, String> {
        let ack = self
            .client
            .turn_start(&TurnStartParams {
                command_id: new_command_id(),
                session_id: self.session_id.clone(),
                input: vec![TurnInputPart::text(text)],
                // Low effort keeps the pay-as-you-go spend down; it has no
                // bearing on approval enforcement.
                reasoning_effort: std::env::var("BAAZ_LIVE_MODES_EFFORT")
                    .ok()
                    .or_else(|| Some("low".into()))
                    .and_then(|level| serde_json::from_value(serde_json::json!(level)).ok()),
                ..Default::default()
            })
            .map_err(|error| format!("turn/start: {error}"))?;
        let mut seen = Seen::default();
        let mut deny_next = deny_first;
        let deadline = Instant::now() + TURN_TIMEOUT;
        while !seen.finished {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return Err(format!("turn {} never completed; asked {:?}", ack.turn_id, seen.asked));
            };
            if let Ok(event) = self.events.recv_timeout(left.min(Duration::from_millis(500))) {
                self.fold(event, &mut seen, &ack.turn_id, &mut deny_next);
            }
        }
        Ok(seen)
    }
}

fn run_cell(cell: &Cell, paths: &Paths, provider_id: Option<&str>, model: Option<&str>) -> Outcome {
    let mut outcome = Outcome::default();
    let client = spawn();
    let events = client.events();
    let opening = cell.from.unwrap_or(cell.mode);
    let started = client.session_start(&SessionStartParams {
        command_id: new_command_id(),
        workspace_root: Some(paths.workspace.to_string_lossy().into_owned()),
        provider_id: provider_id.map(str::to_owned),
        model_id: model.map(str::to_owned),
        approval_mode: Some(wire(opening)),
        permission_profile: profiles().then(|| muse_adapter::permission_profile(opening).to_owned()),
        ..Default::default()
    });
    let session_id = match started {
        Ok(started) => started.session.session_id,
        Err(error) => {
            outcome.error = Some(format!("session/start: {error}"));
            return outcome;
        }
    };
    let mut lane = Lane { client, events, session_id: session_id.clone(), reported: None, decided: HashSet::new() };
    lane.settle(800);
    if let Some(from) = cell.from {
        if lane.reported != Some(from) {
            eprintln!("[{}] opened in {from:?}, reported {:?} before the switch", cell.name, lane.reported);
        }
        outcome.turns += 1;
        match lane.turn(WARMUP, false) {
            Ok(seen) => outcome.warmup_asked = seen.asked.len(),
            Err(error) => {
                outcome.error = Some(format!("warm-up: {error}"));
                outcome.reported = lane.reported;
                return outcome;
            }
        }
        if let Err(error) = lane.client.session_set_approval_mode(&SessionSetApprovalModeParams {
            command_id: new_command_id(),
            session_id: session_id.clone(),
            mode: wire(cell.mode),
        }) {
            outcome.error = Some(format!("session/setApprovalMode: {error}"));
        }
        if profiles() {
            if let Err(error) = lane.client.session_resume(&SessionResumeParams {
                command_id: new_command_id(),
                session_id: session_id.clone(),
                permission_profile: Some(muse_adapter::permission_profile(cell.mode).to_owned()),
                exclude_items: Some(true),
                ..Default::default()
            }) {
                outcome.error = Some(format!("session/resume (profile): {error}"));
            }
        }
        lane.settle(1500);
    }
    outcome.turns += 1;
    match lane.turn(&probe_prompt(paths), cell.deny) {
        Ok(seen) => {
            outcome.asked = seen.asked;
            outcome.answered = seen.answered;
            outcome.reply = tail(&seen.text, 160);
            if !seen.auto_resolved.is_empty() {
                eprintln!("[{}] host-settled approvals (not asks): {:?}", cell.name, seen.auto_resolved);
            }
        }
        Err(error) => outcome.error = Some(error),
    }
    lane.settle(1000);
    outcome.reported = lane.reported;
    outcome.inside = paths.inside.exists();
    outcome.outside = paths.outside.exists();
    lane.client.shutdown();
    outcome
}

#[test]
#[ignore = "spends real (pay-as-you-go) Muse turns; BAAZ_LIVE_MODES=1 to run"]
fn live_modes_muse() {
    if !live_modes::enabled() {
        eprintln!("BAAZ_LIVE_MODES is not 1: nothing spent");
        return;
    }
    let root = live_modes::run_root("muse");
    if std::env::var("BAAZ_LIVE_MODES_LIST").as_deref() == Ok("1") {
        let client = spawn();
        let listed = client.request("model/list", Some(serde_json::json!({}))).expect("model/list");
        println!("{}", serde_json::to_string_pretty(&listed).unwrap_or_default());
        return;
    }
    let provider_id = std::env::var("BAAZ_LIVE_MODES_PROVIDER").ok();
    let model = std::env::var("BAAZ_LIVE_MODES_MODEL").ok();
    let mut results = Vec::new();
    for cell in live_modes::matrix() {
        let paths = live_modes::prepare(&root, "muse", &cell);
        eprintln!("[muse] running {}", cell.name);
        let outcome = run_cell(&cell, &paths, provider_id.as_deref(), model.as_deref());
        eprintln!("[muse] {} → {outcome:?}", cell.name);
        results.push((cell, outcome));
    }
    assert!(live_modes::report("muse", &results), "a Muse cell failed; see the table");
}
