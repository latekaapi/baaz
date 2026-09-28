//! The handoff transcript snapshot: one session, one row (`docs/22-handoff.md` §8).
//!
//! At activation the source view's visible turns — everything its transcript
//! shows, prefix included but the handoff card left out (the divider stands
//! for the handoff) — are written as JSON to
//! `<state>/handoff/<destination>.json`. The destination view then renders,
//! in order: the snapshot's turns (view-side, so provider deltas never touch
//! them), one `HandOff` divider, then its own turns with the pack's user
//! bubble and the pack's one-sentence acknowledgement hidden (the divider
//! stands for both).
//!
//! Deliberately free of gpui: the I/O, the divider text and the turn order
//! are plain unit tests. `aui_protocol` blocks are serde, so the snapshot is
//! just JSON.

use std::path::PathBuf;

use aui_protocol::{Attachment, Block, MarkerKind, ToolBody, Turn, UploadState};
use serde::{Deserialize, Serialize};

use crate::providers::ProviderId;

/// The snapshot schema version. A file with any other version reads as
/// absent: an old build never draws a transcript it cannot parse.
///
/// Additive fields (all `#[serde(default)]`, e.g. `turns_carried`) do not
/// bump this: a file written before the field existed still parses, with
/// the new part omitted wherever it is shown.
pub const SNAPSHOT_VERSION: u32 = 1;

/// The most transcript turns one snapshot keeps: the last this many, oldest
/// dropped with a single "Earlier turns not kept" marker at the top.
/// Snapshots copy the whole transcript and chains nest them, so without a
/// cap the file grows with every hop.
pub const MAX_SNAPSHOT_TURNS: usize = 300;

/// The most lines one text payload keeps in a snapshot: tool outputs and
/// bodies beyond this are truncated with a "… N more lines" line.
pub const MAX_BODY_LINES: usize = 200;

/// The marker turn's text when the turn cap dropped the oldest turns.
pub const EARLIER_TURNS_TEXT: &str = "Earlier turns not kept";

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
    /// minus its own hidden pack bubble and acknowledgement — and never the
    /// handoff card (the divider stands for the handoff). Bounded by
    /// [`bound_turns`] on write and again on read, so old files stay small.
    pub turns: Vec<Turn>,
    /// The pack's verbatim turn count (`pack.recent.len()`) at activation:
    /// the divider's "N turns carried". `None` for files written before the
    /// field existed — the divider then omits the count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns_carried: Option<usize>,
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
/// What lands is bounded ([`bound_turns`]): the live view keeps everything,
/// the file keeps the last [`MAX_SNAPSHOT_TURNS`] turns with capped bodies.
///
/// Under `BAAZ_DETERMINISTIC=1` this is a no-op, the same hermeticity rule
/// the provider-session store follows: a capture must not paint the owner's
/// real transcripts anywhere.
pub fn write_snapshot(destination: &str, snapshot: &HandoffSnapshot) {
    if deterministic() {
        return;
    }
    let mut bounded = snapshot.clone();
    bounded.turns = bound_turns(std::mem::take(&mut bounded.turns));
    if let Ok(bytes) = serde_json::to_vec_pretty(&bounded) {
        let _ = crate::store::write_atomic(&path_for(destination), &bytes);
    }
}

/// Read the snapshot back. Anything unreadable — missing, truncated, a
/// version this build did not write — is `None`, never an error: the caller
/// renders the fallback divider instead. A file from before the frozen card
/// was left out still carries it, so the card is filtered here; an old
/// unbounded file is bounded here too. Both are idempotent, so a file that
/// already went through either reads back unchanged.
pub fn read_snapshot(destination: &str) -> Option<HandoffSnapshot> {
    if deterministic() {
        return None;
    }
    let text = std::fs::read_to_string(path_for(destination)).ok()?;
    let snapshot: HandoffSnapshot = serde_json::from_str(&text).ok()?;
    if snapshot.version != SNAPSHOT_VERSION {
        return None;
    }
    Some(HandoffSnapshot {
        turns: bound_turns(without_handoff_cards(snapshot.turns)),
        ..snapshot
    })
}

/// Whether this is a deterministic capture (see [`write_snapshot`]).
fn deterministic() -> bool {
    std::env::var("BAAZ_DETERMINISTIC").as_deref() == Ok("1")
}

/// The divider's text: "Handed off from \<From> to \<To>", plus the
/// destination model and the pack's verbatim turn count when they are known:
/// "Handed off from Claude Code to Codex · gpt-5 · 4 turns carried". A part
/// this build does not have is omitted, never guessed. Semicolon-free by
/// construction: a `;` inside a `send:` step would split the `--steps` list.
pub fn divider_text(
    from: ProviderId,
    to: ProviderId,
    model: Option<&str>,
    turns_carried: Option<usize>,
) -> String {
    let mut text = format!("Handed off from {} to {}", from.label(), to.label());
    if let Some(model) = model.map(str::trim).filter(|m| !m.is_empty()) {
        text.push_str(&format!(" · {}", model.replace(';', ",")));
    }
    if let Some(count) = turns_carried {
        text.push_str(&format!(" · {count} {} carried", if count == 1 { "turn" } else { "turns" }));
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
/// (where the display map substitutes it). Only the first user turn is ever
/// consulted, so a later message repeating the pack text is real and stays.
/// `None` hides nothing: a first prompt that matches neither is real and
/// stays. The render cache is what applies this; the pack itself stays in
/// history untouched.
pub fn first_pack_user_id(
    own: &[Turn],
    pack_full: Option<&str>,
    pack_display: Option<&str>,
) -> Option<String> {
    let (id, text) = own.iter().find_map(|turn| match turn {
        Turn::User { id, text, .. } => Some((id, text)),
        Turn::Assistant { .. } => None,
    })?;
    let matches = |known: Option<&str>| known.is_some_and(|k| !k.is_empty() && k == text);
    (matches(pack_full) || matches(pack_display)).then(|| id.clone())
}

/// The pack's acknowledgement: the assistant turn immediately following the
/// hidden pack turn — the one-sentence reply the pack's last line asks for
/// ("Reply with a one-sentence acknowledgement of this context…"). Hidden in
/// the live view and on reopen, like the pack bubble; it stays in provider
/// history, and its tokens may still count in the session meters. `None`
/// when the pack turn has no next turn, or the next turn is not an
/// assistant's (a real user message after the pack is never hidden).
pub fn pack_acknowledgement_id(own: &[Turn], pack_id: &str) -> Option<String> {
    let index = own.iter().position(|turn| turn.id() == pack_id)?;
    match own.get(index + 1) {
        Some(Turn::Assistant { id, .. }) => Some(id.clone()),
        _ => None,
    }
}

/// Drop the handoff card (`Block::Handoff`) from snapshot turns: the card
/// captured at activation is frozen at Prepared, with live Cancel and "Open
/// the new session" buttons that no longer mean anything in the
/// destination's history. The divider stands for the handoff, so the card
/// carries nothing the transcript needs. A turn left with no blocks goes
/// entirely; earlier hops' `HandOff` divider markers are untouched.
pub fn without_handoff_cards(turns: Vec<Turn>) -> Vec<Turn> {
    turns
        .into_iter()
        .filter_map(|mut turn| {
            if let Turn::Assistant { blocks, .. } = &mut turn {
                blocks.retain(|block| !matches!(block, Block::Handoff { .. }));
                if blocks.is_empty() {
                    return None;
                }
            }
            Some(turn)
        })
        .collect()
}

/// Cap what a snapshot keeps: each turn's bodies bounded, then at most the
/// last [`MAX_SNAPSHOT_TURNS`] turns with a single [`EARLIER_TURNS_TEXT`]
/// marker at the top. Idempotent — bounding twice is bounding once — so
/// write and read can both apply it. Rendering of what remains is unchanged:
/// this only drops or shortens payloads, never restructures them.
pub fn bound_turns(turns: Vec<Turn>) -> Vec<Turn> {
    let mut bounded: Vec<Turn> = turns.into_iter().map(bound_turn).collect();
    // A leading marker testifies that older turns were already dropped
    // before this call: lift it while re-capping, then put it back when the
    // turns it spoke for are still not all here.
    let had_marker = bounded.first().is_some_and(is_earlier_turns_marker);
    if had_marker {
        bounded.remove(0);
    }
    if bounded.len() > MAX_SNAPSHOT_TURNS {
        bounded.drain(..bounded.len() - MAX_SNAPSHOT_TURNS);
        bounded.insert(0, earlier_turns_marker());
    } else if had_marker {
        bounded.insert(0, earlier_turns_marker());
    }
    bounded
}

/// Whether this turn is the [`bound_turns`] cap marker, recognised by its
/// stable id and text rather than its position.
fn is_earlier_turns_marker(turn: &Turn) -> bool {
    match turn {
        Turn::Assistant { id, blocks, .. } => {
            id == "handoff-earlier-turns"
                && matches!(
                    blocks.as_slice(),
                    [Block::Marker { text, .. }] if text == EARLIER_TURNS_TEXT
                )
        }
        Turn::User { .. } => false,
    }
}

/// The cap marker: one assistant turn holding a single marker, so it rides
/// the same rows the transcript already draws.
fn earlier_turns_marker() -> Turn {
    Turn::Assistant {
        id: "handoff-earlier-turns".to_owned(),
        blocks: vec![Block::Marker {
            kind: MarkerKind::ContextCompacted,
            text: EARLIER_TURNS_TEXT.to_owned(),
        }],
        meta: Default::default(),
        timestamp: None,
    }
}

/// Bound one turn's payloads: long text bodies truncate at
/// [`MAX_BODY_LINES`] with a "… N more lines" line, image and attachment
/// payloads become a placeholder chip, screenshots are dropped. Nested
/// sub-agent transcripts bound the same way (their turn count is their own;
/// only the top level caps at [`MAX_SNAPSHOT_TURNS`]).
fn bound_turn(turn: Turn) -> Turn {
    match turn {
        Turn::User { id, text, attachments, mentions, timestamp } => Turn::User {
            id,
            text,
            attachments: attachments.into_iter().map(bound_attachment).collect(),
            mentions,
            timestamp,
        },
        Turn::Assistant { id, blocks, meta, timestamp } => Turn::Assistant {
            id,
            blocks: blocks.into_iter().map(bound_block).collect(),
            meta,
            timestamp,
        },
    }
}

/// An attachment's payload stays behind: the chip keeps its shape (so the
/// row still draws) with the file's name filed under `meta`, never the
/// bytes behind it.
fn bound_attachment(attachment: Attachment) -> Attachment {
    Attachment {
        name: "[attachment not kept in handoff snapshot]".to_owned(),
        kind: attachment.kind,
        size_bytes: None,
        meta: Some(attachment.name),
        state: UploadState::Ready,
    }
}

fn bound_block(block: Block) -> Block {
    match block {
        Block::Text { text, streaming } => Block::Text { text: truncate_lines(&text), streaming },
        Block::Thinking { text, elapsed_ms, summary, state } => {
            Block::Thinking { text: truncate_lines(&text), elapsed_ms, summary, state }
        }
        Block::ToolCall { id, kind, verb, target, status, duration_ms, body, diff_stat } => {
            Block::ToolCall {
                id,
                kind,
                verb,
                target,
                status,
                duration_ms,
                body: bound_tool_body(body),
                diff_stat,
            }
        }
        Block::ToolGroup { calls, summary, state } => Block::ToolGroup {
            calls: calls
                .into_iter()
                .map(|call| aui_protocol::ToolCall {
                    body: bound_tool_body(call.body),
                    ..call
                })
                .collect(),
            summary,
            state,
        },
        other => other,
    }
}

fn bound_tool_body(body: ToolBody) -> ToolBody {
    match body {
        ToolBody::Shell { output_lines, exit_code, live } => ToolBody::Shell {
            output_lines: truncate_line_list(output_lines),
            exit_code,
            live,
        },
        ToolBody::Mcp { params, result_json } => {
            ToolBody::Mcp { params, result_json: truncate_lines(&result_json) }
        }
        ToolBody::Edit { diff } => ToolBody::Edit { diff: bound_diff(diff) },
        ToolBody::Search { hits } => {
            let mut hits = hits;
            hits.truncate(MAX_BODY_LINES);
            ToolBody::Search { hits }
        }
        ToolBody::Web { results, hidden } => {
            let mut results = results;
            let dropped = results.len().saturating_sub(MAX_BODY_LINES);
            results.truncate(MAX_BODY_LINES);
            ToolBody::Web { results, hidden: hidden.saturating_add(dropped) }
        }
        ToolBody::Browser { action, screenshot: _, caption } => {
            ToolBody::Browser { action, screenshot: None, caption }
        }
        ToolBody::SubAgent { turns } => {
            ToolBody::SubAgent { turns: turns.into_iter().map(bound_turn).collect() }
        }
        other => other,
    }
}

/// A unified diff has no free-text line for the "… N more lines" note, so
/// over-long diffs keep their first [`MAX_BODY_LINES`] rows across hunks
/// (whole-patch `added`/`removed` counts stay as the provider wrote them)
/// and drop the rest. Rendering draws what remains exactly as before.
fn bound_diff(mut diff: aui_protocol::Diff) -> aui_protocol::Diff {
    let mut kept = 0;
    diff.hunks.retain_mut(|hunk| {
        if kept >= MAX_BODY_LINES {
            return false;
        }
        let room = MAX_BODY_LINES - kept;
        if hunk.lines.len() > room {
            hunk.lines.truncate(room);
        }
        kept += hunk.lines.len();
        !hunk.lines.is_empty()
    });
    diff
}

/// Keep the first [`MAX_BODY_LINES`] lines and note what fell off. A text
/// that already ends in our own note is left alone, which is what makes
/// [`bound_turns`] idempotent.
fn truncate_lines(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= MAX_BODY_LINES || lines.last().is_some_and(|last| is_truncated_note(last)) {
        return text.to_owned();
    }
    let mut out = lines[..MAX_BODY_LINES].join("\n");
    out.push('\n');
    out.push_str(&truncated_note(lines.len() - MAX_BODY_LINES));
    out
}

/// The [`truncate_lines`] note over a line list: shell output arrives as one
/// list, so the note is one more line rather than text.
fn truncate_line_list(mut lines: Vec<String>) -> Vec<String> {
    if lines.len() <= MAX_BODY_LINES || lines.last().is_some_and(|last| is_truncated_note(last)) {
        return lines;
    }
    let dropped = lines.len() - MAX_BODY_LINES;
    lines.truncate(MAX_BODY_LINES);
    lines.push(truncated_note(dropped));
    lines
}

fn truncated_note(dropped: usize) -> String {
    format!("… {dropped} more lines")
}

/// Our own truncation note, recognised so a second bound pass leaves the
/// text alone. A person typing this exact line costs nothing: the text is
/// kept whole either way.
fn is_truncated_note(line: &str) -> bool {
    line.starts_with("… ") && line.ends_with(" more lines")
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
    use aui_protocol::AttachmentKind;

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
            turns_carried: Some(3),
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
    fn the_divider_names_both_providers_the_model_and_the_carried_turns() {
        assert_eq!(
            divider_text(ProviderId::ClaudeCode, ProviderId::Codex, Some("gpt-5"), Some(4)),
            "Handed off from Claude Code to Codex · gpt-5 · 4 turns carried"
        );
        assert_eq!(
            divider_text(ProviderId::Muse, ProviderId::ClaudeCode, None, Some(1)),
            "Handed off from Muse to Claude Code · 1 turn carried"
        );
        assert_eq!(
            divider_text(ProviderId::Muse, ProviderId::Codex, Some("  "), None),
            "Handed off from Muse to Codex",
            "a blank model and an unknown count are both omitted"
        );
        assert_eq!(
            divider_text(ProviderId::Muse, ProviderId::Codex, None, None),
            "Handed off from Muse to Codex"
        );
        for text in [
            divider_text(ProviderId::Muse, ProviderId::Codex, Some("m"), Some(2)),
            divider_text(ProviderId::Muse, ProviderId::Codex, Some("a;b"), Some(2)),
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

    #[test]
    fn a_later_message_is_never_hidden_as_the_pack() {
        // Only the destination's FIRST user turn is consulted: the pack went
        // first, so a pack-text match on any later turn is a real message.
        let own = vec![
            user("u1", "A real first prompt"),
            reply("a1", "On it."),
            user("u2", "Continuing a session handed off from Muse. Context follows."),
        ];
        assert_eq!(
            first_pack_user_id(
                &own,
                Some("Continuing a session handed off from Muse. Context follows."),
                Some("Handed off from Muse: x")
            ),
            None,
            "the first user turn is real, so nothing hides even though a later one matches"
        );
    }

    #[test]
    fn the_pack_acknowledgement_is_the_assistant_turn_right_after_the_pack() {
        let own = vec![
            user("u-pack", "pack"),
            reply("a-ack", "Context received; I'll wait for your next message."),
            user("u2", "Keep going"),
            reply("a2", "Going."),
        ];
        assert_eq!(
            pack_acknowledgement_id(&own, "u-pack").as_deref(),
            Some("a-ack"),
            "the pack's one-sentence reply hides with the pack"
        );
        assert_eq!(
            pack_acknowledgement_id(&own, "u2").as_deref(),
            Some("a2"),
            "any turn's immediate assistant follower resolves the same way"
        );
    }

    #[test]
    fn no_acknowledgement_hides_without_an_assistant_follower() {
        assert_eq!(
            pack_acknowledgement_id(&[user("u-pack", "pack")], "u-pack"),
            None,
            "the pack with no reply yet hides nothing more"
        );
        let own = vec![user("u-pack", "pack"), user("u2", "a real follow-up")];
        assert_eq!(
            pack_acknowledgement_id(&own, "u-pack"),
            None,
            "a real user message after the pack is never hidden"
        );
        assert_eq!(pack_acknowledgement_id(&own, "no-such-turn"), None);
    }

    fn handoff_card_turn() -> Turn {
        Turn::Assistant {
            id: "handoff-abc".to_owned(),
            blocks: vec![Block::Handoff {
                id: "handoff-abc".to_owned(),
                from: aui_protocol::Provider::Claude,
                to: aui_protocol::Provider::Codex,
                from_model: String::new(),
                to_model: "gpt-5".to_owned(),
                state: aui_protocol::HandoffState::Prepared,
                carried: vec![],
                lost: vec![],
                pack_tokens: None,
                destination_session: None,
            }],
            meta: Default::default(),
            timestamp: None,
        }
    }

    #[test]
    fn the_frozen_card_leaves_the_snapshot_but_the_divider_stays() {
        let card = handoff_card_turn();
        let divider = divider_turn(
            "handoff-divider-d",
            ProviderId::ClaudeCode,
            ProviderId::Codex,
            "Handed off from Claude Code to Codex".to_owned(),
        );
        let turns = vec![user("u1", "Plan Greyport"), card, divider.clone(), reply("a1", "On it.")];
        let kept = without_handoff_cards(turns);
        assert_eq!(kept.len(), 3, "only the card turn goes");
        assert!(
            kept.iter().all(|turn| !turn
                .blocks()
                .iter()
                .any(|block| matches!(block, Block::Handoff { .. }))),
            "no handoff card survives, wherever it sat"
        );
        assert!(
            kept.iter().any(|turn| turn.id() == "handoff-divider-d"),
            "the earlier hop's divider marker is not a card and stays"
        );
    }

    #[test]
    fn a_read_filters_a_card_from_a_snapshot_already_on_disk() {
        let _env = EnvLock::hold("old-card");
        let mut snapshot = snapshot("dest-card");
        snapshot.turns.push(handoff_card_turn());
        // Written raw, as the previous build left it: the card on disk.
        let bytes = serde_json::to_vec_pretty(&snapshot).expect("old snapshot serializes");
        std::fs::create_dir_all(path_for("dest-card").parent().expect("handoff dir")).expect("dir");
        std::fs::write(path_for("dest-card"), &bytes).expect("old snapshot writes");
        let back = read_snapshot("dest-card").expect("old snapshot still reads");
        assert!(
            back.turns.iter().all(|turn| !turn
                .blocks()
                .iter()
                .any(|block| matches!(block, Block::Handoff { .. }))),
            "the frozen card filters on read"
        );
        assert_eq!(back.turns_carried, Some(3), "the additive field survives beside the filter");
    }

    #[test]
    fn a_snapshot_without_a_turn_count_still_reads() {
        let _env = EnvLock::hold("old-nocount");
        // A file from before `turns_carried` existed carries no such key.
        let raw = serde_json::json!({
            "version": SNAPSHOT_VERSION,
            "from": "muse",
            "to": "codex",
            "source": "src-1",
            "activated_ms": 1_759_999_999_999i64,
            "pack_text": "pack",
            "pack_display": "display",
            "turns": [],
        });
        std::fs::create_dir_all(path_for("dest-nc").parent().expect("handoff dir")).expect("dir");
        std::fs::write(
            path_for("dest-nc"),
            serde_json::to_vec_pretty(&raw).expect("old shape serializes"),
        )
        .expect("old shape writes");
        let back = read_snapshot("dest-nc").expect("additive fields never break old files");
        assert_eq!(back.turns_carried, None, "the divider then omits the count");
    }

    fn long_text(lines: usize) -> String {
        (0..lines).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn long_bodies_truncate_with_a_more_lines_note() {
        let text = long_text(MAX_BODY_LINES + 50);
        let turn = Turn::Assistant {
            id: "a-long".to_owned(),
            blocks: vec![Block::Text { text: text.clone(), streaming: false }],
            meta: Default::default(),
            timestamp: None,
        };
        let bounded = bound_turns(vec![turn]);
        let kept = match &bounded[0] {
            Turn::Assistant { blocks, .. } => match &blocks[0] {
                Block::Text { text, .. } => text.clone(),
                other => panic!("text stays text, got {other:?}"),
            },
            Turn::User { .. } => panic!("the turn stays an assistant turn"),
        };
        let kept_lines: Vec<&str> = kept.lines().collect();
        assert_eq!(kept_lines.len(), MAX_BODY_LINES + 1);
        assert_eq!(kept_lines[MAX_BODY_LINES], "… 50 more lines");
        assert!(kept_lines[0].starts_with("line 0"), "the head is what stays");
        // Idempotent: bounding the bounded changes nothing.
        let again = bound_turns(bounded.clone());
        assert_eq!(again, bounded, "write and read can both bound safely");
    }

    #[test]
    fn shell_output_screenshots_and_attachments_stay_small() {
        let turn = Turn::Assistant {
            id: "a-tools".to_owned(),
            blocks: vec![
                Block::tool_call(aui_protocol::ToolCall {
                    id: "tc-shell".to_owned(),
                    kind: aui_protocol::ToolKind::Shell,
                    verb: "Ran".to_owned(),
                    target: "make logs".to_owned(),
                    status: aui_protocol::ToolStatus::Success,
                    duration_ms: Some(1),
                    body: ToolBody::Shell {
                        output_lines: (0..MAX_BODY_LINES + 10)
                            .map(|n| format!("out {n}"))
                            .collect(),
                        exit_code: Some(0),
                        live: false,
                    },
                    diff_stat: None,
                }),
                Block::tool_call(aui_protocol::ToolCall {
                    id: "tc-shot".to_owned(),
                    kind: aui_protocol::ToolKind::Browser,
                    verb: "Clicked".to_owned(),
                    target: "a page".to_owned(),
                    status: aui_protocol::ToolStatus::Success,
                    duration_ms: Some(1),
                    body: ToolBody::Browser {
                        action: "Clicked".to_owned(),
                        screenshot: Some("data:image/png;base64,AAAA".to_owned()),
                        caption: None,
                    },
                    diff_stat: None,
                }),
            ],
            meta: Default::default(),
            timestamp: None,
        };
        let with_attachment = Turn::User {
            id: "u-img".to_owned(),
            text: "see this".to_owned(),
            attachments: vec![Attachment {
                name: "harbor.png".to_owned(),
                kind: AttachmentKind::Image,
                size_bytes: Some(1_000_000),
                meta: None,
                state: UploadState::Ready,
            }],
            mentions: vec![],
            timestamp: None,
        };
        let bounded = bound_turns(vec![turn, with_attachment]);
        match &bounded[0] {
            Turn::Assistant { blocks, .. } => {
                match &blocks[0] {
                    Block::ToolCall { body: ToolBody::Shell { output_lines, .. }, .. } => {
                        assert_eq!(output_lines.len(), MAX_BODY_LINES + 1);
                        assert_eq!(output_lines[MAX_BODY_LINES], "… 10 more lines");
                    }
                    other => panic!("shell stays shell, got {other:?}"),
                }
                match &blocks[1] {
                    Block::ToolCall { body: ToolBody::Browser { screenshot, .. }, .. } => {
                        assert_eq!(screenshot, &None, "the screenshot bytes stay behind");
                    }
                    other => panic!("browser stays browser, got {other:?}"),
                }
            }
            Turn::User { .. } => panic!("the tool turn stays an assistant turn"),
        }
        match &bounded[1] {
            Turn::User { attachments, .. } => {
                assert_eq!(attachments.len(), 1, "one chip still draws");
                assert_eq!(attachments[0].size_bytes, None);
                assert!(
                    attachments[0].name.contains("not kept"),
                    "a placeholder chip, got {}",
                    attachments[0].name
                );
            }
            Turn::Assistant { .. } => panic!("the user turn stays a user turn"),
        }
    }

    #[test]
    fn only_the_last_300_turns_keep_with_one_marker_on_top() {
        let turns: Vec<Turn> =
            (0..MAX_SNAPSHOT_TURNS + 40).map(|n| user(&format!("u{n}"), "hi")).collect();
        let bounded = bound_turns(turns);
        assert_eq!(bounded.len(), MAX_SNAPSHOT_TURNS + 1, "300 kept plus the one marker");
        match &bounded[0] {
            Turn::Assistant { blocks, .. } => match &blocks[0] {
                Block::Marker { text, .. } => assert_eq!(text, EARLIER_TURNS_TEXT),
                other => panic!("the top turn is the marker, got {other:?}"),
            },
            Turn::User { .. } => panic!("the top turn is the marker"),
        }
        assert_eq!(bounded[1].id(), "u40", "the oldest kept turn follows the marker");
        assert_eq!(
            bounded[bounded.len() - 1].id().to_owned(),
            format!("u{}", MAX_SNAPSHOT_TURNS + 39)
        );
        // Idempotent: a capped snapshot re-caps to itself.
        let again = bound_turns(bounded.clone());
        assert_eq!(again, bounded, "no second marker ever stacks");
    }

    #[test]
    fn a_short_snapshot_caps_nothing_and_marks_nothing() {
        let turns = vec![user("u1", "hi"), reply("a1", "hello")];
        assert_eq!(bound_turns(turns.clone()), turns);
    }

    #[test]
    fn what_is_written_is_bounded_and_card_free() {
        let _env = EnvLock::hold("bounded-write");
        let mut snapshot = snapshot("dest-bound");
        snapshot.turns.push(handoff_card_turn());
        snapshot.turns.push(Turn::Assistant {
            id: "a-long".to_owned(),
            blocks: vec![Block::Text { text: long_text(MAX_BODY_LINES + 5), streaming: false }],
            meta: Default::default(),
            timestamp: None,
        });
        write_snapshot("dest-bound", &snapshot);
        let back = read_snapshot("dest-bound").expect("bounded snapshot reads");
        assert!(
            back.turns.iter().all(|turn| !turn
                .blocks()
                .iter()
                .any(|block| matches!(block, Block::Handoff { .. }))),
            "the card never lands in the file"
        );
        let long = back.turns.iter().find(|turn| turn.id() == "a-long").expect("long turn kept");
        match long {
            Turn::Assistant { blocks, .. } => match &blocks[0] {
                Block::Text { text, .. } => {
                    assert_eq!(text.lines().count(), MAX_BODY_LINES + 1, "the file keeps the bound body");
                }
                other => panic!("text stays text, got {other:?}"),
            },
            Turn::User { .. } => panic!("the turn stays an assistant turn"),
        }
    }
}
