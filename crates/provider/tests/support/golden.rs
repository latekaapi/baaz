//! Golden transcripts (docs/82 5.3): a fixture folded through an adapter's
//! fold, recorded as the `Session` the app would hold, so a rewrite of the
//! fold can be held to byte-identical output.
//!
//! Included by `#[path]` from each adapter's `tests/golden.rs`. One golden
//! file per fixture holds three things:
//!
//! * `final`: every session the fixture folded, pretty-printed — the
//!   transcript the app ends with;
//! * `steps`: `<line>:<hash>` after every input line that changed a
//!   session — so the live rendering, not only the end state, is pinned;
//! * `abandon`: `<line>:<hash>` of the sessions after the child is lost at
//!   that line (the adapter's child-loss settlement, run on a copy).
//!
//! Wall-clock readings are not reproducible, so [`Mask`] scrubs the fields
//! that carry them before anything is hashed or written. Regenerate with
//! `UPDATE_GOLDEN=1` — only as a deliberate, named step.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aui_protocol::{Delta, Provider, Session};
use serde_json::Value;

/// How wall-clock fields are scrubbed before hashing.
#[derive(Clone, Copy, Debug)]
pub enum Mask {
    /// Replay without live timing: `elapsed_ms` (thinking, activity, todo)
    /// and an allowed approval's `duration_ms` fall back to a wall clock
    /// when the wire timed nothing. A replay folds in microseconds, so any
    /// such reading under a second is scrubbed to 0.
    Replay,
    /// Live timing on: every `duration_ms`, `elapsed_ms` and `timestamp`
    /// keeps only whether it is present.
    Live,
}

fn scrub(value: &mut Value, mask: Mask) {
    match value {
        Value::Object(map) => {
            let approval_outcome = map.contains_key("exit_code") && map.contains_key("duration_ms");
            for (key, field) in map.iter_mut() {
                match (mask, key.as_str()) {
                    (Mask::Replay, "elapsed_ms") => {
                        if field.as_u64().is_some_and(|ms| ms < 1000) {
                            *field = Value::from(0);
                        }
                    }
                    (Mask::Replay, "duration_ms") if approval_outcome => {
                        if field.as_u64().is_some_and(|ms| ms < 1000) {
                            *field = Value::from(0);
                        }
                    }
                    (Mask::Live, "duration_ms" | "elapsed_ms" | "timestamp") => {
                        if !field.is_null() {
                            *field = Value::from("set");
                        }
                    }
                    _ => scrub(field, mask),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| scrub(item, mask)),
        _ => {}
    }
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Sessions keyed by id, built from the deltas a fold emits.
pub struct Recorder {
    provider: Provider,
    mask: Mask,
    sessions: BTreeMap<String, Session>,
    last: u64,
    steps: Vec<String>,
    abandon: Vec<String>,
}

impl Recorder {
    pub fn new(provider: Provider, mask: Mask) -> Self {
        Self { provider, mask, sessions: BTreeMap::new(), last: 0, steps: Vec::new(), abandon: Vec::new() }
    }

    /// Apply `deltas` to the session `session_id`, opening it first.
    pub fn apply(&mut self, session_id: &str, deltas: Vec<Delta>) {
        apply_to(&mut self.sessions, self.provider, session_id, deltas);
    }

    fn value_of(&self, sessions: &BTreeMap<String, Session>) -> Value {
        let mut value = serde_json::to_value(sessions).expect("sessions serialise");
        scrub(&mut value, self.mask);
        value
    }

    fn value(&self) -> Value {
        self.value_of(&self.sessions)
    }

    fn hash_of(&self, sessions: &BTreeMap<String, Session>) -> u64 {
        fnv(serde_json::to_string(&self.value_of(sessions)).expect("value serialises").as_bytes())
    }

    fn hash(&self) -> u64 {
        self.hash_of(&self.sessions)
    }

    /// Close input line `line`: record a step when any session moved.
    pub fn step(&mut self, line: usize) {
        let hash = self.hash();
        if hash != self.last {
            self.last = hash;
            self.steps.push(format!("{line}:{hash:016x}"));
        }
    }

    /// The child was lost after line `line`: `deltas` are what the
    /// adapter's settlement emitted on a copy of the fold, per session.
    pub fn abandon(&mut self, line: usize, deltas: Vec<(String, Vec<Delta>)>) {
        let mut sessions = self.sessions.clone();
        for (session_id, deltas) in deltas {
            apply_to(&mut sessions, self.provider, &session_id, deltas);
        }
        let hash = self.hash_of(&sessions);
        self.abandon.push(format!("{line}:{hash:016x}"));
    }

    /// Compare against (or with `UPDATE_GOLDEN` set, write) the golden at
    /// `path`. Returns a description of the first difference, if any.
    pub fn check(&self, path: &Path) -> Option<String> {
        let actual = serde_json::json!({
            "final": self.value(),
            "steps": self.steps,
            "abandon": self.abandon,
        });
        let text = format!("{}\n", serde_json::to_string_pretty(&actual).expect("golden serialises"));
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::create_dir_all(path.parent().expect("golden dir")).expect("golden dir");
            std::fs::write(path, &text).expect("golden written");
            return None;
        }
        let expected = match std::fs::read_to_string(path) {
            Ok(expected) => expected,
            Err(err) => return Some(format!("{}: {err} (run with UPDATE_GOLDEN=1)", path.display())),
        };
        if expected == text {
            return None;
        }
        let expected: Value = serde_json::from_str(&expected).expect("golden parses");
        for key in ["final", "steps", "abandon"] {
            if expected[key] != actual[key] {
                let detail = match (key, &expected[key], &actual[key]) {
                    ("final", _, _) => first_line_diff(
                        &serde_json::to_string_pretty(&expected[key]).unwrap_or_default(),
                        &serde_json::to_string_pretty(&actual[key]).unwrap_or_default(),
                    ),
                    (_, Value::Array(want), Value::Array(got)) => {
                        let at = want.iter().zip(got.iter()).position(|(w, g)| w != g).unwrap_or(want.len().min(got.len()));
                        format!(
                            "first difference at entry {at}: want {:?}, got {:?} ({} vs {} entries)",
                            want.get(at),
                            got.get(at),
                            want.len(),
                            got.len()
                        )
                    }
                    _ => String::new(),
                };
                return Some(format!("{}: `{key}` differs: {detail}", path.display()));
            }
        }
        Some(format!("{}: differs in layout only", path.display()))
    }
}

fn apply_to(sessions: &mut BTreeMap<String, Session>, provider: Provider, session_id: &str, deltas: Vec<Delta>) {
    let session =
        sessions.entry(session_id.to_owned()).or_insert_with(|| Session::new(session_id, provider, "", ""));
    for delta in deltas {
        session.apply(delta);
    }
}

fn first_line_diff(want: &str, got: &str) -> String {
    for (number, (w, g)) in want.lines().zip(got.lines()).enumerate() {
        if w != g {
            return format!("line {}: want `{}`, got `{}`", number + 1, w.trim(), g.trim());
        }
    }
    format!("lengths differ ({} vs {} lines)", want.lines().count(), got.lines().count())
}

/// Every `*.jsonl` under `dir`, sorted.
pub fn fixtures(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("{}: {err}", dir.display()))
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no fixtures in {}", dir.display());
    paths
}

/// Where the golden for `fixture` lives under `golden_dir`.
pub fn golden_path(golden_dir: &Path, fixture: &Path) -> PathBuf {
    let stem = fixture.file_stem().expect("named fixture").to_string_lossy().into_owned();
    golden_dir.join(format!("{stem}.json"))
}
