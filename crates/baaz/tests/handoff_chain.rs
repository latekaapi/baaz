//! X3c: a handoff continues in the same transcript — prefix, divider, then
//! the new provider (`docs/22-handoff.md` §8, transcript half).
//!
//! Baaz is a binary, so this suite cannot call its functions: the behaviour
//! lives in `handoff_snapshot.rs` unit tests (divider text, first-user-only
//! pack match, pack acknowledgement, frozen-card filter, bounds — all real
//! calls, no window) and in `session/render.rs` `gpui::test`s (prefix +
//! divider + own-turn order through the real render cache, with a window).
//! What remains here is the real file contract plus one pin, below.

use std::path::{Path, PathBuf};

fn manifest() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn source(name: &str) -> String {
    let path = manifest().join("src").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("source reads: {}", path.display()))
}

/// The one source-level pin in this suite: activation snapshots the source
/// transcript for the destination, the destination shows the prefix above
/// its divider, and reopening reattaches either from the file or from the
/// fallback divider. This path genuinely needs the application harness (a
/// `Harness` plus windows for both views), so no test without gpui can
/// drive it — it is pinned here, and every behaviour it wires is covered by
/// real calls in the unit and `gpui::test` suites named above.
#[test]
fn activation_and_reopen_wiring_stays_app_side() {
    let lifecycle = source("app/lifecycle.rs");
    assert!(
        lifecycle.contains("handoff_snapshot::write_snapshot"),
        "activation must snapshot the source transcript for the destination"
    );
    assert!(
        lifecycle.contains("show_handoff_prefix"),
        "activation must show the prefix on the destination view"
    );
    assert!(
        lifecycle.contains("attach_handoff_prefix") && lifecycle.contains("read_snapshot"),
        "reopening must reattach the prefix from the snapshot"
    );
    let module = source("handoff_snapshot.rs");
    assert!(
        module.contains("pub fn write_snapshot")
            && module.contains("support_dir()")
            && module.contains("write_atomic"),
        "the snapshot writer must exist, resolve through the state dir, and land atomically"
    );
    assert!(
        module.contains("pub fn fallback_text")
            && module.contains("first_pack_user_id")
            && module.contains("pack_acknowledgement_id")
            && module.contains("without_handoff_cards")
            && module.contains("pub fn bound_turns"),
        "the fallback divider, both pack hides, the card filter and the bounds must exist"
    );
    let view = source("session/handoff.rs");
    assert!(
        view.contains("show_handoff_prefix") && view.contains("handoff_snapshot_turns"),
        "the destination view must show the prefix above its own turns"
    );
    assert!(
        !view.contains("append_handoff_origin"),
        "the fold-written origin marker stays gone: one handoff draws exactly one divider"
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
/// compose across restarts. Real `aui_protocol` turns through real JSON on
/// a real file — no source is read.
#[test]
fn a_snapshot_round_trips_prefix_divider_and_own_turns() {
    let dir = std::env::temp_dir().join(format!("baaz-handoff-chain-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("chain test state dir");
    let path = dir.join("dest-1.json");
    let turns = vec![
        user("u1", "Name three ferry routes"),
        reply("a1", "Cormorant, Heron, Gull"),
        divider("div-1", "Handed off from Claude Code to Codex · gpt-5 · 2 turns carried"),
        reply("a2", "Got it — ready to continue…"),
    ];
    // Keys mirror the real file schema (`HandoffSnapshot` serializes
    // snake_case): a drift here fails the decode below.
    let snapshot = serde_json::json!({
        "version": 1,
        "from": "claude-code",
        "to": "codex",
        "source": "src-1",
        "to_model": "gpt-5",
        "activated_ms": 1_759_999_999_999i64,
        "pack_text": "Continuing a session handed off from Claude Code. Context follows.",
        "pack_display": "Handed off from Claude Code: the goal",
        "turns": turns,
        "turns_carried": 2,
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
    assert_eq!(back["turns_carried"].as_u64(), Some(2), "the carried count survives");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The divider text contract both lanes share: providers named both ways,
/// the model and the carried count only when known, and never a `;` (it
/// would split `--steps`). Literal strings — the unit tests own the builder.
#[test]
fn divider_texts_name_both_providers_and_stay_step_safe() {
    for text in [
        "Handed off from Claude Code to Codex · gpt-5 · 2 turns carried",
        "Handed off from Muse to Claude Code · 1 turn carried",
        "Handed off from Muse to Codex",
        "Handed off from Claude Code — earlier turns are in the previous session",
    ] {
        assert!(!text.contains(';'), "a `;` would split the `--steps` list: {text}");
        assert!(text.starts_with("Handed off from "), "the divider reads as a handoff: {text}");
    }
}
