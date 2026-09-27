//! The handoff transcript snapshot: one session, one row (`docs/22-handoff.md` §8).
//!
//! At activation the source view's visible turns — everything its transcript
//! shows, including any prefix it itself carried and its handoff card — are
//! written as JSON to `<state>/handoff/<destination>.json`. The destination
//! view then renders, in order: the snapshot's turns (view-side, so provider
//! deltas never touch them), one `HandOff` divider, then its own turns with
//! the pack's user bubble hidden (the divider stands for it).
//!
//! Deliberately free of gpui: the I/O, the divider text and the turn order
//! are plain unit tests. `aui_protocol` blocks are serde, so the snapshot is
//! just JSON.

use std::path::PathBuf;

use aui_protocol::{Block, MarkerKind, Turn};
use serde::{Deserialize, Serialize};

use crate::providers::ProviderId;

/// The snapshot schema version. A file with any other version reads as
/// absent: an old build never draws a transcript it cannot parse.
pub const SNAPSHOT_VERSION: u32 = 1;

/// The pack's first line, naming the source provider. Reopening without a
/// snapshot recognises a pack-derived bubble by this prefix on the
/// display-map key, so even a pre-snapshot pair hides the pack once.
pub const PACK_HEAD_PREFIX: &str = "Continuing a session handed off from ";

/// One handoff hop, as the destination replays it: the source transcript at
/// activation, plus the facts the divider and the pack-bubble hide need.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HandoffSnapshot {
    /// [`SNAPSHOT_VERSION`]: what wrote this file.
    pub version: u32,
    /// The source lane's wire id (`"muse"`, `"claude-code"`, `"codex"`).
    pub from: String,
    /// The destination lane's wire id.
    pub to: String,
    /// The source session id: what the divider's back-link opens.
    pub source: String,
    /// The destination model at activation, when it was known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_model: Option<String>,
    /// When the run activated, as Unix milliseconds.
    pub activated_ms: i64,
    /// The pack's full model-visible text: what the live user bubble holds.
    pub pack_text: String,
    /// The pack's short summary: what a replayed user bubble holds (the
    /// display map substitutes it on replay, see
    /// [`crate::provider_sessions`]).
    pub pack_display: String,
    /// The source view's visible turns at activation, oldest first — its
    /// prefix (so chains of any length compose), its divider, its own turns
    /// including the handoff card, minus its own hidden pack bubble.
    pub turns: Vec<Turn>,
}

/// `<state>/handoff/<destination>.json`, honouring `BAAZ_STATE_DIR` through
/// the same resolver the other stores use. The id is sanitised: it becomes
/// a file name, never a path.
pub fn path_for(destination: &str) -> PathBuf {
    crate::store::support_dir().join("handoff").join(format!("{}.json", sanitize(destination)))
}

/// The file-name half of [`path_for`]: ids are uuids, but one `/` would
/// escape the directory, so anything outside a small set flattens.
fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Write the snapshot atomically (temp file + rename). Best-effort like the
/// other stores: an unwritable state dir loses history, never the session.
///
/// Under `BAAZ_DETERMINISTIC=1` this is a no-op, the same hermeticity rule
/// the provider-session store follows: a capture must not paint the owner's
/// real transcripts anywhere.
pub fn write_snapshot(destination: &str, snapshot: &HandoffSnapshot) {
    if deterministic() {
        return;
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(snapshot) {
        let _ = crate::store::write_atomic(&path_for(destination), &bytes);
    }
}

/// Read the snapshot back. Anything unreadable — missing, truncated, a
/// version this build did not write — is `None`, never an error: the caller
/// renders the fallback divider instead.
pub fn read_snapshot(destination: &str) -> Option<HandoffSnapshot> {
    if deterministic() {
        return None;
    }
    let text = std::fs::read_to_string(path_for(destination)).ok()?;
    let snapshot: HandoffSnapshot = serde_json::from_str(&text).ok()?;
    (snapshot.version == SNAPSHOT_VERSION).then_some(snapshot)
}

/// Whether this is a deterministic capture (see [`write_snapshot`]).
fn deterministic() -> bool {
    std::env::var("BAAZ_DETERMINISTIC").as_deref() == Ok("1")
}

/// The divider's text: "Handed off from \<From> to \<To>", plus the
/// destination model when it is known. Semicolon-free by construction: a
/// `;` inside a `send:` step would split the `--steps` list.
pub fn divider_text(from: ProviderId, to: ProviderId, model: Option<&str>) -> String {
    let mut text = format!("Handed off from {} to {}", from.label(), to.label());
    if let Some(model) = model.filter(|m| !m.trim().is_empty()) {
        text.push_str(&format!(" · {model}"));
    }
    text
}

/// The divider when no snapshot exists: the chain is known (the record kept
/// `handoff_from`) but the earlier turns are not on disk. Plain text, no
/// link of its own — the marker's existing back-link (wired from the origin
/// the caller also sets) is what opens the previous session.
pub fn fallback_text(from: ProviderId) -> String {
    format!("Handed off from {} — earlier turns are in the previous session", from.label())
}

/// The divider as a turn: one assistant turn holding the single marker, so
/// it rides the same rows the transcript already draws. `id` must be stable
/// per destination (it keys the virtual list's row sync).
pub fn divider_turn(id: &str, from: ProviderId, to: ProviderId, text: String) -> Turn {
    Turn::Assistant {
        id: id.to_owned(),
        blocks: vec![Block::Marker {
            kind: MarkerKind::HandOff {
                from: crate::handoff::wire_provider(from),
                to: crate::handoff::wire_provider(to),
            },
            text,
        }],
        meta: Default::default(),
        timestamp: None,
    }
}

/// The pack bubble's turn id: the destination's FIRST user turn when it
/// matches either pack text — the full text live, the summary after a replay
/// (where the display map substitutes it). `None` hides nothing: a first
/// prompt that matches neither is real and stays. The render cache is what
/// applies this; the pack itself stays in history untouched.
pub fn first_pack_user_id(
    own: &[Turn],
    pack_full: Option<&str>,
    pack_display: Option<&str>,
) -> Option<String> {
    own.iter().find_map(|turn| match turn {
        Turn::User { id, text, .. } => {
            let matches = |known: Option<&str>| known.is_some_and(|k| !k.is_empty() && k == text);
            (matches(pack_full) || matches(pack_display)).then(|| id.clone())
        }
        Turn::Assistant { .. } => None,
    })
}

/// Now, as Unix milliseconds: the snapshot's activation time.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Holds [`crate::store::test_env_lock`] while a test points
    /// `BAAZ_STATE_DIR` at a temp dir (and clears `BAAZ_DETERMINISTIC`),
    /// restoring both after: both variables are process-global. Same shape
    /// as the provider-session store's `EnvLock`.
    struct EnvLock {
        _guard: std::sync::MutexGuard<'static, ()>,
        state_dir: Option<std::ffi::OsString>,
        deterministic: Option<std::ffi::OsString>,
    }

    impl EnvLock {
        fn hold(name: &str) -> Self {
            let locked = Self {
                _guard: crate::store::test_env_lock(),
                state_dir: std::env::var_os("BAAZ_STATE_DIR"),
                deterministic: std::env::var_os("BAAZ_DETERMINISTIC"),
            };
            let dir = std::env::temp_dir().join(format!("baaz-handoff-snap-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("handoff snapshot test state dir");
            std::env::set_var("BAAZ_STATE_DIR", &dir);
            std::env::remove_var("BAAZ_DETERMINISTIC");
            locked
        }
    }

    impl Drop for EnvLock {
        fn drop(&mut self) {
            match &self.state_dir {
                Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
                None => std::env::remove_var("BAAZ_STATE_DIR"),
            }
            match &self.deterministic {
                Some(value) => std::env::set_var("BAAZ_DETERMINISTIC", value),
                None => std::env::remove_var("BAAZ_DETERMINISTIC"),
            }
        }
    }

    fn user(id: &str, text: &str) -> Turn {
        Turn::User {
            id: id.to_owned(),
            text: text.to_owned(),
            attachments: vec![],
            mentions: vec![],
            timestamp: None,
        }
    }

    fn reply(id: &str, text: &str) -> Turn {
        Turn::Assistant {
            id: id.to_owned(),
            blocks: vec![Block::Text { text: text.to_owned(), streaming: false }],
            meta: Default::default(),
            timestamp: None,
        }
    }

    fn snapshot(dest: &str) -> HandoffSnapshot {
        let _ = dest;
        HandoffSnapshot {
            version: SNAPSHOT_VERSION,
            from: ProviderId::ClaudeCode.as_str().to_owned(),
            to: ProviderId::Codex.as_str().to_owned(),
            source: "src-1".to_owned(),
            to_model: Some("gpt-5".to_owned()),
            activated_ms: 1_759_999_999_999,
            pack_text: "Continuing a session handed off from Claude Code. Context follows.".to_owned(),
            pack_display: "Handed off from Claude Code: the goal (3 recent turns, 0 open todos, 1 files touched)"
                .to_owned(),
            turns: vec![user("u1", "Name three ferry routes"), reply("a1", "Cormorant, Heron, Gull")],
        }
    }

    #[test]
    fn a_snapshot_round_trips_through_the_file() {
        let _env = EnvLock::hold("roundtrip");
        let snapshot = snapshot("dest-1");
        write_snapshot("dest-1", &snapshot);
        let back = read_snapshot("dest-1").expect("what was written reads back");
        assert_eq!(back, snapshot, "write, read back identical blocks");
    }

    #[test]
    fn a_missing_snapshot_reads_as_absent() {
        let _env = EnvLock::hold("missing");
        assert_eq!(read_snapshot("never-written"), None, "no crash, no empty gap: None");
    }

    #[test]
    fn a_foreign_version_reads_as_absent() {
        let _env = EnvLock::hold("version");
        let mut snapshot = snapshot("dest-v");
        snapshot.version = SNAPSHOT_VERSION + 1;
        write_snapshot("dest-v", &snapshot);
        assert_eq!(read_snapshot("dest-v"), None, "a version this build did not write never draws");
    }

    #[test]
    fn a_deterministic_capture_neither_writes_nor_reads() {
        let _env = EnvLock::hold("hermetic");
        std::env::set_var("BAAZ_DETERMINISTIC", "1");
        write_snapshot("dest-d", &snapshot("dest-d"));
        assert_eq!(read_snapshot("dest-d"), None);
        assert!(!path_for("dest-d").exists(), "hermetic: nothing lands in the owner's store");
    }

    #[test]
    fn a_destination_id_never_escapes_its_directory() {
        let _env = EnvLock::hold("sanitize");
        let path = path_for("../evil");
        assert_eq!(path.file_name().and_then(|n| n.to_str()), Some("___evil.json"));
        assert!(path.parent().is_some_and(|p| p.ends_with("handoff")));
    }

    #[test]
    fn the_divider_names_both_providers_and_the_model_when_known() {
        assert_eq!(
            divider_text(ProviderId::ClaudeCode, ProviderId::Codex, Some("gpt-5")),
            "Handed off from Claude Code to Codex · gpt-5"
        );
        assert_eq!(
            divider_text(ProviderId::Muse, ProviderId::ClaudeCode, None),
            "Handed off from Muse to Claude Code"
        );
        assert_eq!(
            divider_text(ProviderId::Muse, ProviderId::Codex, Some("  ")),
            "Handed off from Muse to Codex",
            "a blank model is the same as unknown"
        );
        for text in [
            divider_text(ProviderId::Muse, ProviderId::Codex, Some("m")),
            fallback_text(ProviderId::Muse),
        ] {
            assert!(!text.contains(';'), "a `;` would split the `--steps` list");
        }
    }

    #[test]
    fn the_fallback_divider_names_the_source_and_the_previous_session() {
        assert_eq!(
            fallback_text(ProviderId::ClaudeCode),
            "Handed off from Claude Code — earlier turns are in the previous session"
        );
    }

    #[test]
    fn the_pack_bubble_is_found_live_by_its_full_text() {
        let own = vec![
            user("u-pack", "Continuing a session handed off from Claude Code. Context follows."),
            reply("a1", "Got it — ready to continue…"),
            user("u2", "Keep going"),
        ];
        assert_eq!(
            first_pack_user_id(
                &own,
                Some("Continuing a session handed off from Claude Code. Context follows."),
                Some("Handed off from Claude Code: the goal")
            )
            .as_deref(),
            Some("u-pack")
        );
    }

    #[test]
    fn the_pack_bubble_is_found_replayed_by_its_summary_text() {
        // A replay substitutes the summary for the whole pack (the display
        // map), so the bubble matches `pack_display`, never `pack_text`.
        let own = vec![
            user("u-pack", "Handed off from Muse: the goal"),
            reply("a1", "Got it — ready to continue…"),
        ];
        assert_eq!(
            first_pack_user_id(
                &own,
                Some("Continuing a session handed off from Muse. Context follows."),
                Some("Handed off from Muse: the goal")
            )
            .as_deref(),
            Some("u-pack")
        );
    }

    #[test]
    fn a_first_prompt_that_is_not_the_pack_hides_nothing() {
        let own = vec![user("u1", "A real first prompt"), reply("a1", "On it.")];
        assert_eq!(
            first_pack_user_id(
                &own,
                Some("Continuing a session handed off from Muse"),
                Some("Handed off from Muse: x")
            ),
            None,
            "nothing matches the pack, nothing hides"
        );
    }

    #[test]
    fn only_the_first_matching_user_turn_hides() {
        let own = vec![user("u-pack", "pack"), reply("a1", "ack"), user("u2", "pack")];
        assert_eq!(
            first_pack_user_id(&own, Some("pack"), Some("other")).as_deref(),
            Some("u-pack"),
            "a later turn repeating the pack text is real and stays"
        );
    }
}
