//! Muse mode picks through a hub lane, against a stub `muse serve`: a pick
//! moves the approval mode AND the sandbox permission profile (a
//! metadata-only `session/resume`), ordered so a half-applied pick never
//! leaves the chip reading less access than the session has; a resumed
//! session's profile is matched to its mode; a fork keeps the parent's
//! mode. No model is ever reached.

use std::path::PathBuf;

use aui_protocol::PermissionMode;
use muse_client::{MuseClient, MuseConfig};
use provider::{Ack, Command, ConnectInfo, Provider, ProviderAdapter, ProviderEvent};
use provider_muse::MuseHub;

struct Stub {
    log: PathBuf,
    hub: MuseHub,
    lane: Provider,
}

fn stub(name: &str, flags: &[&str]) -> Stub {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mode_stub.py");
    let log = std::env::temp_dir().join(format!("provider-muse-modes-{name}-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&log);
    let mut args: Vec<String> = flags.iter().map(|flag| flag.to_string()).collect();
    args.push(log.to_string_lossy().into_owned());
    let client = MuseClient::spawn(&MuseConfig { program: script, trust_workspace: false, no_session_log: true, extra_args: args })
        .expect("stub spawns — is python3 on PATH?");
    let hub = MuseHub::from_client(client);
    let mut lane = hub.lane();
    lane.connect(&ConnectInfo::new("baaz", "0.1.0")).expect("handshake");
    Stub { log, hub, lane: Provider::new(lane) }
}

impl Drop for Stub {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.log);
    }
}

impl Stub {
    fn resume(&self) {
        let ack = self
            .lane
            .send(Command::ResumeSession {
                request_id: "r-1".into(),
                session_id: "s-1".into(),
                cursor: None,
                metadata_only: true,
                mode: None,
            })
            .expect("resume");
        assert!(matches!(ack, Ack::Session { .. }));
    }

    fn pick(&self, mode: PermissionMode) -> Result<Ack, provider::ProviderError> {
        self.lane.send(Command::SelectApprovalMode { request_id: "m-1".into(), session_id: "s-1".into(), mode })
    }

    /// The mode and profile calls after the attach, as `(method, mode or profile)`.
    fn calls(&self) -> Vec<(String, String)> {
        let text = std::fs::read_to_string(&self.log).unwrap_or_default();
        text.lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter_map(|v| {
                let method = v.get("method")?.as_str()?.to_owned();
                let arg = match method.as_str() {
                    "session/setApprovalMode" => v.pointer("/params/mode"),
                    "session/resume" => v.pointer("/params/permissionProfile"),
                    _ => return None,
                }?;
                Some((method, arg.as_str()?.to_owned()))
            })
            .collect()
    }
}

fn call(method: &str, arg: &str) -> (String, String) {
    (method.to_owned(), arg.to_owned())
}

#[test]
fn a_pick_that_raises_access_moves_the_mode_then_the_profile() {
    let stub = stub("raise", &[]);
    stub.resume();
    stub.pick(PermissionMode::AllowAll).expect("pick");
    assert_eq!(stub.calls(), [call("session/setApprovalMode", "allowAll"), call("session/resume", ":unrestricted")]);
}

#[test]
fn a_pick_that_lowers_access_moves_the_profile_first_and_says_what_holds() {
    let stub = stub("lower", &["fail-mode"]);
    stub.resume();
    let refused = stub.pick(PermissionMode::DenyUnmatched).expect_err("the mode step is refused");
    assert_eq!(stub.calls(), [call("session/resume", ":read-only"), call("session/setApprovalMode", "denyUnmatched")]);
    assert!(refused.to_string().contains("Read-only sandbox is on"), "{refused}");
}

#[test]
fn a_refused_profile_puts_the_raised_mode_back() {
    let stub = stub("rollback", &["fail-resume"]);
    stub.resume();
    let refused = stub.pick(PermissionMode::AllowAll).expect_err("the profile is refused");
    assert_eq!(
        stub.calls(),
        [
            call("session/setApprovalMode", "allowAll"),
            call("session/resume", ":unrestricted"),
            call("session/setApprovalMode", "onRequest"),
        ]
    );
    assert!(refused.to_string().contains("stays in Auto"), "{refused}");
}

#[test]
fn a_resumed_session_under_another_profile_is_matched_to_its_mode() {
    let matching = stub("reconcile-match", &["mode=denyUnmatched", "profile=:read-only"]);
    matching.resume();
    assert!(matching.calls().is_empty(), "a match sends nothing");
    let other = stub("reconcile", &["mode=denyUnmatched", "profile=:auto-review"]);
    other.resume();
    assert_eq!(other.calls(), [call("session/resume", ":read-only")]);
}

#[test]
fn a_refused_profile_match_falls_back_to_read_only_and_says_so() {
    let stub = stub("reconcile-fail", &["mode=promptUnmatched", "profile=:unrestricted", "fail-resume", "allow-read-only"]);
    stub.resume();
    assert_eq!(stub.calls(), [call("session/resume", ":ask-me"), call("session/resume", ":read-only")]);
    let notice = stub.lane.events().try_iter().find_map(|event| match event {
        ProviderEvent::Notice { text, .. } => Some(text),
        _ => None,
    });
    assert!(notice.is_some_and(|text| text.contains("now runs read-only")));
}

#[test]
fn a_fork_keeps_the_parents_mode_profile_first() {
    let stub = stub("fork", &["mode=denyUnmatched", "profile=:read-only"]);
    stub.resume();
    let ack = stub
        .lane
        .send(Command::ForkSession {
            request_id: "f-1".into(),
            session_id: "s-1".into(),
            through_turn: Some("t-1".into()),
            metadata_only: true,
            mode: Some(PermissionMode::DenyUnmatched),
        })
        .expect("fork");
    assert!(matches!(ack, Ack::Session { ref session_id, .. } if session_id == "s-fork"));
    assert_eq!(stub.calls(), [call("session/resume", ":read-only"), call("session/setApprovalMode", "denyUnmatched")]);
    let _ = &stub.hub;
}
