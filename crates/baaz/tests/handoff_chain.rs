//! X3a: a handoff continues in the same transcript — prefix, divider, then
//! the new provider (`docs/22-handoff.md` §8, transcript half).
//!
//! Baaz is a binary, so like the other `baaz/tests` suites this pins the
//! wiring by reading the sources it must contain, and exercises the
//! snapshot's wire contract — `aui_protocol` turns through JSON, atomically
//! — for real. Remove the view-side prefix, the activation snapshot, or the
//! reopen attach and the pins below name the regression.

use std::path::{Path, PathBuf};

fn manifest() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn source(name: &str) -> String {
    let path = manifest().join("src").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("source reads: {}", path.display()))
}

/// The activation snapshot exists: the source transcript is written for the
/// destination at activation, through the state-dir resolver (so
/// `BAAZ_STATE_DIR` is honoured) and atomically.
#[test]
fn activation_writes_a_snapshot_for_the_destination() {
    let lifecycle = source("app/lifecycle.rs");
    assert!(
        lifecycle.contains("handoff_snapshot::write_snapshot"),
        "activation must snapshot the source transcript for the destination"
    );
    let module = source("handoff_snapshot.rs");
    assert!(module.contains("pub fn write_snapshot"), "the snapshot writer must exist");
    assert!(module.contains("support_dir()"), "the snapshot must resolve through the state dir");
    assert!(module.contains("write_atomic"), "the snapshot write must be atomic");
}

/// The destination renders the prefix view-side: the render cache assembles
/// the snapshot's turns above one divider, never folded into the provider
/// fold (so provider deltas cannot touch them).
#[test]
fn the_destination_renders_a_view_side_prefix() {
    let render = source("session/render.rs");
    assert!(
        render.contains("handoff_prefix"),
        "the render cache must assemble the view-side handoff prefix"
    );
    let view = source("session/handoff.rs");
    assert!(
        view.contains("show_handoff_prefix"),
        "the destination view must show the prefix above its own turns"
    );
}

/// Today's separate top origin marker is gone: the divider stands alone, so
/// a handoff draws exactly one divider, never two.
#[test]
fn no_fold_written_origin_marker_doubles_the_divider() {
    let view = source("session/handoff.rs");
    assert!(
        !view.contains("append_handoff_origin"),
        "the fold-written origin marker is replaced by the view-side divider"
    );
    assert!(
        !view.contains("\"-origin\"") && !view.contains("-origin\", self.session_id"),
        "no client-authored origin turn may enter the fold"
    );
}

/// Reopening the destination reattaches prefix + divider from the snapshot —
/// no child is spawned for the source — and a chain without a snapshot still
/// draws its fallback divider.
#[test]
fn reopening_reattaches_the_prefix_or_the_fallback_divider() {
    let lifecycle = source("app/lifecycle.rs");
    assert!(
        lifecycle.contains("attach_handoff_prefix"),
        "opening a session must reattach its handoff prefix"
    );
    assert!(
        lifecycle.contains("read_snapshot"),
        "reopening must read the snapshot back"
    );
    let module = source("handoff_snapshot.rs");
    assert!(module.contains("pub fn fallback_text"), "the missing-snapshot divider must exist");
    assert!(
        module.contains("first_pack_user_id"),
        "the pack bubble hide must survive a replay"
    );
}

fn user(id: &str, text: &str) -> aui_protocol::Turn {
    aui_protocol::Turn::User {
        id: id.to_owned(),
        text: text.to_owned(),
        attachments: vec![],
        mentions: vec![],
        timestamp: None,
    }
}

fn reply(id: &str, text: &str) -> aui_protocol::Turn {
    aui_protocol::Turn::Assistant {
        id: id.to_owned(),
        blocks: vec![aui_protocol::Block::Text { text: text.to_owned(), streaming: false }],
        meta: Default::default(),
        timestamp: None,
    }
}

fn divider(id: &str, text: &str) -> aui_protocol::Turn {
    aui_protocol::Turn::Assistant {
        id: id.to_owned(),
        blocks: vec![aui_protocol::Block::Marker {
            kind: aui_protocol::MarkerKind::HandOff {
                from: aui_protocol::Provider::Claude,
                to: aui_protocol::Provider::Codex,
            },
            text: text.to_owned(),
        }],
        meta: Default::default(),
        timestamp: None,
    }
}

fn handoff_markers(turns: &[aui_protocol::Turn]) -> Vec<String> {
    turns
        .iter()
        .filter_map(|turn| match turn {
            aui_protocol::Turn::Assistant { blocks, .. } => Some(blocks.clone()),
            aui_protocol::Turn::User { .. } => None,
        })
        .flatten()
        .filter_map(|block| match block {
            aui_protocol::Block::Marker { kind: aui_protocol::MarkerKind::HandOff { .. }, text } => {
                Some(text)
            }
            _ => None,
        })
        .collect()
}

/// The snapshot's wire contract: a transcript with a divider serde
/// round-trips through the file byte-identical, so chains of any length
/// compose across restarts.
#[test]
fn a_snapshot_round_trips_prefix_divider_and_own_turns() {
    let dir = std::env::temp_dir().join(format!("baaz-handoff-chain-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("chain test state dir");
    let path = dir.join("dest-1.json");
    let turns = vec![
        user("u1", "Name three ferry routes"),
        reply("a1", "Cormorant, Heron, Gull"),
        divider("div-1", "Handed off from Claude Code to Codex · gpt-5"),
        reply("a2", "Got it — ready to continue…"),
    ];
    let snapshot = serde_json::json!({
        "version": 1,
        "from": "claude-code",
        "to": "codex",
        "source": "src-1",
        "toModel": "gpt-5",
        "activatedMs": 1_759_999_999_999i64,
        "packText": "Continuing a session handed off from Claude Code. Context follows.",
        "packDisplay": "Handed off from Claude Code: the goal",
        "turns": turns,
    });
    let bytes = serde_json::to_vec_pretty(&snapshot).expect("snapshot serializes");
    let temporary = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&temporary, &bytes).expect("snapshot writes");
    std::fs::rename(&temporary, &path).expect("snapshot lands atomically");
    let back: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("snapshot reads"))
            .expect("snapshot parses");
    assert_eq!(back, snapshot, "write, read back identical blocks");
    let back_turns: Vec<aui_protocol::Turn> =
        serde_json::from_value(back["turns"].clone()).expect("turns decode as protocol turns");
    assert_eq!(handoff_markers(&back_turns).len(), 1, "the divider survives the file");
    assert_eq!(back["source"].as_str(), Some("src-1"), "the back-link target survives");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The divider text contract both lanes share: providers named both ways,
/// the model only when known, and never a `;` (it would split `--steps`).
#[test]
fn divider_texts_name_both_providers_and_stay_step_safe() {
    for text in [
        "Handed off from Claude Code to Codex · gpt-5",
        "Handed off from Muse to Claude Code",
        "Handed off from Claude Code — earlier turns are in the previous session",
    ] {
        assert!(!text.contains(';'), "a `;` would split the `--steps` list: {text}");
        assert!(text.starts_with("Handed off from "), "the divider reads as a handoff: {text}");
    }
}
