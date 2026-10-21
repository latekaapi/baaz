//! Wire captures as neutral events, for `--replay`, `--bench` and captures.
//!
//! A capture (`fixtures/msp/*.jsonl`) holds both directions of a real
//! connection. [`Capture::load`] reads the server half into raw events —
//! unpacking `view/page` results into the notifications they carry — and
//! the client half into the prompts each turn was sent with. [`Steps`]
//! folds the raw events one at a time through a fresh [`MuseFold`] into the
//! same [`ProviderEvent`]s a hub lane delivers, so a replayed session draws
//! through exactly the path a live one does.
//!
//! [`ReadOnly`] is the lane a replay or an offline view rides: it answers
//! the catalog and pending pulls with nothing and refuses every command
//! that would act, saying why.

use std::path::Path;

use crossbeam_channel::{unbounded, Receiver, Sender};
use muse_adapter::MuseFold;
use muse_client::{frame, MuseEvent};
use provider::{
    Ack, Capability, CapabilitySet, CapabilityState, Command, ConnectInfo, Handshake, ProviderAdapter,
    ProviderError, ProviderEvent, ProviderId,
};

use crate::facts::FactTracker;

/// A capture file, read.
pub struct Capture {
    raw: Vec<MuseEvent>,
    /// `(request id, words)` for each `turn/start` the capture sent: what an
    /// error card's retry and the first-prompt title read.
    pub sent: Vec<(String, String)>,
}

impl Capture {
    /// Read `path`. Lines that do not frame are logged to stderr and skipped.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let mut raw = Vec::new();
        let mut sent = Vec::new();
        for (number, line) in text.lines().enumerate() {
            if let Some(body) = line.strip_prefix("--> ") {
                sent.extend(submitted_text(body));
                continue;
            }
            let Some(body) = line.strip_prefix("<-- ") else { continue };
            match frame::parse_line(body) {
                // A `view/page` result carries its events whole: a capture of
                // a real open path is mostly one of those.
                Ok(Some(frame::Frame::Response { result, .. })) => raw.extend(page_result_events(&result)),
                Ok(Some(frame)) => raw.extend(MuseEvent::from_frame(frame)),
                Ok(None) => {}
                Err(error) => eprintln!("{}:{}: {error}", path.display(), number + 1),
            }
        }
        Ok(Self { raw, sent })
    }

    /// The session the capture names first, if it names one.
    pub fn session_id(&self) -> Option<String> {
        self.raw.iter().find_map(|event| match event {
            MuseEvent::Notification { session_id: Some(id), .. } => Some(id.clone()),
            _ => None,
        })
    }

    /// How many raw events it holds.
    pub fn len(&self) -> usize {
        self.raw.len()
    }

    /// Whether it holds none.
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// Drop everything from the last `turn/completed` on: the session then
    /// replays with that turn still running.
    pub fn cut_before_last_completion(&mut self) {
        let last = self.raw.iter().rposition(|event| {
            matches!(event, MuseEvent::Notification { method, .. } if method == "turn/completed")
        });
        if let Some(at) = last {
            self.raw.truncate(at);
        }
    }

    /// The capture as a stream of neutral events, one raw event at a time.
    pub fn steps(self) -> Steps {
        Steps { raw: self.raw.into_iter(), folder: Folder::new() }
    }

    /// The whole capture as neutral events, every session's (a view keeps
    /// its own).
    pub fn events(self) -> Vec<ProviderEvent> {
        self.steps().flatten().collect()
    }
}

/// A capture folded one raw event at a time.
pub struct Steps {
    raw: std::vec::IntoIter<MuseEvent>,
    folder: Folder,
}

impl Iterator for Steps {
    type Item = Vec<ProviderEvent>;

    /// The neutral events the next raw event folds to (possibly none).
    fn next(&mut self) -> Option<Self::Item> {
        let event = self.raw.next()?;
        Some(self.folder.fold(event))
    }
}

/// The translation a hub lane runs, for events that come from a file or a
/// test rather than a child: one fold, one fact tracker.
#[derive(Default)]
pub struct Folder {
    fold: MuseFold,
    tracker: FactTracker,
}

impl Folder {
    /// A folder that has folded nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// One server→client line of a capture (`<--` stripped), folded.
    /// A `view/page` result folds the notifications it carries.
    pub fn fold_line(&mut self, body: &str) -> Vec<ProviderEvent> {
        let raw = match frame::parse_line(body) {
            Ok(Some(frame::Frame::Response { result, .. })) => page_result_events(&result),
            Ok(Some(frame)) => MuseEvent::from_frame(frame).into_iter().collect(),
            _ => Vec::new(),
        };
        raw.into_iter().flat_map(|event| self.fold(event)).collect()
    }

    /// One raw transport event, folded.
    pub fn fold(&mut self, event: MuseEvent) -> Vec<ProviderEvent> {
        let (method, params) = match &event {
            MuseEvent::Notification { method, params, .. } => (method.clone(), params.clone()),
            MuseEvent::ServerRequest { method, params, .. } => (method.clone(), params.clone()),
            MuseEvent::Closed(_) => return Vec::new(),
        };
        let deltas = self.fold.apply(event);
        let Some(session) = self.fold.last_touched().map(str::to_owned) else { return Vec::new() };
        let mut out = Vec::new();
        if !deltas.is_empty() {
            out.push(ProviderEvent::Deltas { session_id: Some(session.clone()), deltas });
        }
        out.extend(self.tracker.after(&mut self.fold, &session, &method, &params));
        out
    }
}

/// The notifications a `view/page` result holds, in page order.
fn page_result_events(result: &serde_json::Value) -> Vec<MuseEvent> {
    let Some(events) = result.get("events").and_then(|v| v.as_array()) else { return Vec::new() };
    events
        .iter()
        .filter_map(|event| {
            let method = event.get("method")?.as_str()?.to_owned();
            let params = event.get("params").cloned().unwrap_or(serde_json::Value::Null);
            MuseEvent::from_frame(frame::Frame::Notification { method, params })
        })
        .collect()
}

/// The `(commandId, text)` a captured `turn/start` was sent with:
/// `displayText` when present, else the text parts joined.
fn submitted_text(line: &str) -> Option<(String, String)> {
    let frame: serde_json::Value = serde_json::from_str(line).ok()?;
    if frame.get("method")?.as_str()? != "turn/start" {
        return None;
    }
    let params = frame.get("params")?;
    let command_id = params.get("commandId")?.as_str()?.to_owned();
    let text = match params.get("displayText").and_then(|v| v.as_str()) {
        Some(text) => text.to_owned(),
        None => params
            .get("input")?
            .as_array()?
            .iter()
            .filter_map(|part| part.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(""),
    };
    (!text.is_empty()).then_some((command_id, text))
}

/// The lane a replayed capture or an offline view rides: nothing behind it.
/// The catalog and the pending pulls answer empty; anything that would act
/// is refused with `reason`.
pub struct ReadOnly {
    reason: String,
    tx: Sender<ProviderEvent>,
    rx: Receiver<ProviderEvent>,
}

impl ReadOnly {
    /// A read-only lane refusing with `reason`.
    pub fn new(reason: impl Into<String>) -> Self {
        let (tx, rx) = unbounded();
        Self { reason: reason.into(), tx, rx }
    }
}

impl ProviderAdapter for ReadOnly {
    fn id(&self) -> ProviderId {
        ProviderId::Muse
    }

    fn connect(&mut self, _client: &ConnectInfo) -> Result<Handshake, ProviderError> {
        Ok(Handshake { provider: ProviderId::Muse, agent_name: "muse".into(), agent_version: "replay".into() })
    }

    fn capabilities(&self) -> CapabilitySet {
        let quiet = [Capability::ModelCatalog, Capability::Approvals];
        CapabilitySet::new(Capability::all().map(|capability| {
            let state = if quiet.contains(&capability) {
                CapabilityState::Native
            } else {
                CapabilityState::Unavailable { reason: self.reason.clone() }
            };
            (capability, state)
        }))
    }

    fn dispatch(&self, command: Command) -> Result<Ack, ProviderError> {
        match command {
            Command::ListModels { .. } => Ok(Ack::ModelCatalog { models: Vec::new(), provider: String::new() }),
            Command::ListPending { .. } => Ok(Ack::PendingWork { approvals: Vec::new(), questions: Vec::new() }),
            other => Err(ProviderError::unsupported(other.capability(), self.reason.clone())),
        }
    }

    fn events(&self) -> Receiver<ProviderEvent> {
        // Never sends: the replayed events are folded by the caller.
        let _ = &self.tx;
        self.rx.clone()
    }

    fn shutdown(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/msp").join(name)
    }

    #[test]
    fn a_capture_replays_to_its_turns_and_its_sent_prompts() {
        let capture = Capture::load(&fixture("synthetic-markdown.jsonl")).expect("capture");
        assert_eq!(capture.session_id().as_deref(), Some("synthetic-markdown"));
        let events = capture.events();
        assert!(events.iter().any(|e| matches!(e, ProviderEvent::Deltas { .. })), "transcript deltas");
        assert!(events.iter().any(|e| matches!(e, ProviderEvent::TurnSettled { .. })), "the turn settles");
    }

    #[test]
    fn a_cut_capture_leaves_its_last_turn_running() {
        let mut capture = Capture::load(&fixture("synthetic-markdown.jsonl")).expect("capture");
        capture.cut_before_last_completion();
        let events = capture.events();
        assert!(events.iter().any(|e| matches!(e, ProviderEvent::TurnRunning { .. })));
        assert!(!events.iter().any(|e| matches!(e, ProviderEvent::TurnSettled { .. })));
    }

    #[test]
    fn the_read_only_lane_answers_pulls_and_refuses_acts() {
        let lane = provider::Provider::new(ReadOnly::new("a replayed capture is read-only"));
        assert!(lane.send(Command::ListModels { session: None }).is_ok());
        let refused = lane.send(Command::CompactSession { request_id: "r".into(), session_id: "s".into(), through_turn: None });
        assert!(matches!(refused, Err(ProviderError::Unsupported { ref reason, .. }) if reason.contains("read-only")));
    }
}
