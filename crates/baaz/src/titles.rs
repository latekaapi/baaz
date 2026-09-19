//! Generated session titles (auto-titles).
//!
//! Muse never generates titles: the index holds either the literal `New
//! session` or an echo of the first prompt. So Baaz asks
//! once, with ONE call to the cheapest model, on the first send, in a
//! throwaway side session — and the real transcript stays clean because the
//! billed turn runs under a different session id entirely.
//!
//! The mechanism, end to end:
//!
//! * [`should_title`] decides, on the first `turn/started`, whether this
//!   session earns a generation. Exactly one per session, ever.
//! * the app starts a side session (`session/start` in the same workspace,
//!   `modelId` pinned, a bare-UUIDv7 client id recorded before the start),
//!   sends ONE short prompt ([`title_prompt`]) for a 3–6 word title, and
//!   `turn/completed` with a free `session/read` ([`harvest_title_text`]).
//! * the result lands in `sessions.json` as `generated_title`, ranked under
//!   a user-given name in the label order; the server record is never
//!   renamed. The side session is marked `hidden` the moment it starts, so
//!   it never reaches the sidebar, the palette or the counts.
//! * failure is silent and cheap: a timeout ([`TITLE_TIMEOUT_SECS`]), a wire
//!   error, or a missing model falls back to today's first-prompt label.
//!   One log line, never a dialog, at most one retry per session.
//! * a reply that lands after the timeout stood the job down is still
//!   harvested ([`should_land_late`]): the turn is already paid for, so a
//!   free `session/read` lands it exactly like an in-time answer — dropped
//!   only when the session is gone or has since been named, and never
//!   retried into a second generation ([`should_retry_title`]).
//!
//! Spend: one short turn per unnamed session, against the login's tier —
//! never on resume, reconnect, replay, restart, or a second turn. The
//! billed evidence for that claim is one fresh-session send plus this call.

use crate::sessions::SessionMeta;

/// The model a title generation is pinned to at `session/start` when the
/// catalog carries it: the non-contributor row. The wire reports `cost:
/// null` for every model, so this is a smallest-appropriate pick by proxy
/// (newest generation) with a privacy basis (the `-contributor` description
/// flags transcript text "may be used for product improvement"), not a
/// priced one. Absent from `model/list`, the start omits `modelId` and takes
/// the server default — the send path never hard-fails on a model id.
pub const TITLE_MODEL_ID: &str = "muse-spark-1.3";

/// How long a title turn may run before the app stops waiting for it: 90 s.
///
/// Grounded in this login's own side-session logs, not a guess. The billed
/// title turn of 2026-09-17 ran 20_673 ms server-side — its answer text
/// committed at 3_409 ms while the end-of-turn gate ate the other 17_102 ms —
/// so the old 20 s ceiling stood the job down a fraction of a second before
/// `turn/completed` arrived and the paid-for answer was discarded. The real
/// turn beside it ran 36_774 ms, and another real turn on 2026-09-16 ran
/// 20_760 ms: the slow tail is server-side (first token to gate), not prompt
/// length, so a short title prompt earns no shorter ceiling. 90 s clears the
/// slowest turn seen (~2.5×) and the title sample (~4×) while still reaping
/// a stuck side session: the row falls back to the first prompt at the
/// deadline, and a reply that lands later is harvested, never retried (see
/// [`should_land_late`]), so a timeout can never double-bill. Nothing ever
/// waits on it: the real turn does not block, and the row shows the pending
/// placeholder meanwhile.
pub const TITLE_TIMEOUT_SECS: u64 = 90;

/// Attempts per session, total, this run: the first try plus at most one
/// retry on a wire error. A timeout never retries (the turn may still be
/// running server-side), and neither does a late harvest after a stand-down
/// (the turn is already paid for — see [`should_retry_title`]). Across
/// restarts the persisted `title_attempted` marker holds the line at one.
pub const TITLE_MAX_ATTEMPTS: u8 = 2;

/// Whether a failed title attempt earns the one retry: an in-time failure on
/// the first try does, nothing else. A stand-down — the watchdog's timeout,
/// or a harvest running after one — never retries: the turn may still be
/// running server-side (timeout) or is already paid for (late harvest), so
/// another `turn/start` would double-bill a turn this session already owns.
pub fn should_retry_title(tries: u8, stood_down: bool) -> bool {
    !stood_down && tries < TITLE_MAX_ATTEMPTS
}

/// The naming half of the late-harvest question: a stood-down reply still
/// applies unless the session has since been named. `None` is "nothing
/// known" (landing would mint a stray override for a dead id); a user-given
/// name wins over a generated title, so naming one wins over landing one.
/// The caller additionally requires the row itself to still exist — an
/// override can outlive a closed session, and that is gone too.
pub fn should_land_late(meta: Option<&SessionMeta>) -> bool {
    meta.is_some_and(|meta| meta.name.is_none())
}

/// The title prompt carries the gist, not the transcript: the first message
/// past this many characters is cut. Bounds the billed prompt to ~200
/// tokens of instruction plus a short quote.
pub const TITLE_PROMPT_CHARS: usize = 500;

/// A fresh client id for one title side session: a bare UUIDv7 command id —
/// exactly the shape the server mints itself when `sessionId` is omitted
/// (and the shape `session/start` accepts: 36 characters, where the old
/// namespaced id failed with `invalid length: found 50`). Minting
/// client-side rather than letting the server assign one means the id is
/// known before the start runs, so the explicit record (memory plus the
/// `side_session` override) lands first and a crash between the start and
/// the hide still hides by record. Two generations never share an identity.
pub fn side_session_id() -> String {
    muse_client::new_command_id()
}

/// Whether this session earns a title generation now: the first
/// `turn/started` of a session with no name and no generated title.
///
/// False when the switch is off, when there is no client (replay,
/// `--no-connect`), for a side session itself (by the caller's explicit
/// record — this module never inspects id shapes), for a session that
/// already has a name, a generated title, or turns (a resume, a reconnect,
/// a restart), and for one that already attempted — which is what makes
/// the "exactly one generation per session, ever" rule hold across
/// restarts.
pub fn should_title(
    auto_on: bool,
    has_client: bool,
    meta: Option<&SessionMeta>,
    turns: u64,
    is_side: bool,
) -> bool {
    if !auto_on || !has_client || turns != 0 || is_side {
        return false;
    }
    match meta {
        None => true,
        Some(meta) => meta.name.is_none() && meta.generated_title.is_none() && !meta.title_attempted,
    }
}

/// The `modelId` for the side session's `session/start`: the pinned id when
/// the catalog carries it, else `None` — omit the field and take the server
/// default rather than failing the send path on a model id.
pub fn pick_title_model(models: &[muse_client::schema::ModelCatalogEntry]) -> Option<String> {
    models
        .iter()
        .any(|m| m.model_id == TITLE_MODEL_ID)
        .then(|| TITLE_MODEL_ID.to_owned())
}

/// The one short prompt the side session sends: the user's first message,
/// quoted and bounded, asking for a 3–6 word title and nothing else.
pub fn title_prompt(first_message: &str) -> String {
    let quoted: String = first_message.chars().take(TITLE_PROMPT_CHARS).collect();
    format!(
        "Suggest a short title, 3 to 6 words, for a chat session that started with this user message:\n\n{quoted}\n\nReply with only the title: no quotes, no trailing punctuation, no explanation."
    )
}

/// A model reply as a row's title: collapsed, unquoted, cut the row's own
/// way. `None` is "nothing usable" — the caller falls back to the first
/// prompt, silently.
pub fn clean_title(reply: &str) -> Option<String> {
    let flat: String = reply.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat = flat.trim().trim_matches(['"', '\'', '“', '”']).trim();
    let flat = flat.strip_prefix("Title:").map(str::trim).unwrap_or(flat);
    let flat = flat.trim_matches(['"', '\'', '“', '”', '.', '!', ':']).trim();
    if flat.is_empty() {
        return None;
    }
    Some(crate::sidebar::one_line(flat))
}

/// The side session's answer, off a free `session/read`: the newest
/// `agentMessage` text, inline items then the snapshot, the same two shapes
/// `first_shell_command` already reads. `None` is "no text yet" — retry the
/// read once a later `turn/completed` arrives, or give up per the attempt
/// budget; never a dialog.
pub fn harvest_title_text(read: &muse_client::schema::SessionReadResult) -> Option<String> {
    let inline = read.history.items.iter().flatten();
    let snapshot = read.history.snapshot.iter().flat_map(|s| s.state.items.iter());
    inline
        .chain(snapshot)
        .filter(|item| item.kind == muse_client::schema::ItemKind::AgentMessage)
        .filter_map(|item| item.text.as_deref())
        .rfind(|text| !text.trim().is_empty())
        .and_then(clean_title)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta_with(name: Option<&str>, generated: Option<&str>, attempted: bool) -> SessionMeta {
        SessionMeta {
            name: name.map(str::to_owned),
            generated_title: generated.map(str::to_owned),
            title_attempted: attempted,
            ..Default::default()
        }
    }

    #[test]
    fn a_fresh_first_turn_earns_exactly_one_generation() {
        assert!(should_title(true, true, None, 0, false));
    }

    #[test]
    fn the_switch_off_means_no_model_call_ever() {
        assert!(!should_title(false, true, None, 0, false));
    }

    #[test]
    fn replay_and_offline_runs_never_title() {
        assert!(!should_title(true, false, None, 0, false));
    }

    #[test]
    fn named_sessions_keep_their_name() {
        let meta = meta_with(Some("Ship it"), None, false);
        assert!(!should_title(true, true, Some(&meta), 0, false));
    }

    #[test]
    fn a_generated_title_is_never_regenerated() {
        let meta = meta_with(None, Some("Tighten validation"), false);
        assert!(!should_title(true, true, Some(&meta), 0, false));
    }

    #[test]
    fn sessions_with_turns_are_resumes_not_first_sends() {
        assert!(!should_title(true, true, None, 3, false));
    }

    #[test]
    fn an_attempt_is_forever_one_run_later() {
        let meta = meta_with(None, None, true);
        assert!(!should_title(true, true, Some(&meta), 0, false));
    }

    #[test]
    fn the_timeout_clears_the_slowest_turn_seen() {
        // Grounded 2026-09-17: the billed title turn ran 20_673 ms, the real
        // turns beside it 36_774 ms and 20_760 ms — so 90 s (≈2.5× the
        // slowest, ≈4× the title sample), still a stuck-job guard.
        assert_eq!(TITLE_TIMEOUT_SECS, 90);
    }

    #[test]
    fn a_late_answer_lands_on_a_live_unnamed_session() {
        let meta = meta_with(None, None, true);
        assert!(should_land_late(Some(&meta)));
    }

    #[test]
    fn a_late_answer_is_dropped_once_the_session_is_named() {
        let meta = meta_with(Some("Ship it"), None, true);
        assert!(!should_land_late(Some(&meta)));
    }

    #[test]
    fn a_late_answer_is_dropped_when_the_session_is_gone() {
        assert!(!should_land_late(None));
    }

    #[test]
    fn an_in_time_failure_retries_once_on_the_first_try() {
        assert!(should_retry_title(1, false));
        assert!(!should_retry_title(2, false));
    }

    #[test]
    fn a_timeout_never_retries_on_any_try() {
        assert!(!should_retry_title(1, true));
        assert!(!should_retry_title(2, true));
    }

    #[test]
    fn a_late_harvest_never_retries_the_paid_for_turn() {
        assert!(!should_retry_title(1, true));
        assert!(!should_retry_title(TITLE_MAX_ATTEMPTS, true));
    }

    #[test]
    fn the_once_ever_marker_outlives_any_retry_budget() {
        // A stand-down keeps the job for a late harvest but never clears the
        // marker: whatever the try count, an attempted session earns nothing.
        let meta = meta_with(None, None, true);
        assert!(!should_title(true, true, Some(&meta), 0, false));
        assert!(!should_retry_title(TITLE_MAX_ATTEMPTS, true));
    }

    #[test]
    fn a_side_session_never_titles_itself() {
        // Recognition is the caller's explicit record, not the id shape: a
        // side id is a bare uuid, exactly like a real session's.
        assert!(!should_title(true, true, None, 0, true));
        assert!(should_title(true, true, None, 0, false));
    }

    #[test]
    fn side_session_ids_are_bare_uuids_and_never_repeat() {
        let first = side_session_id();
        let second = side_session_id();
        assert_ne!(first, second);
        for id in [&first, &second] {
            // 36 characters, uuid-shaped: what `session/start` accepts, where
            // the old 50-character namespaced id failed `invalid length`.
            assert_eq!(id.len(), 36);
            assert!(id.parse::<uuid::Uuid>().is_ok());
        }
    }

    #[test]
    fn the_pinned_model_wins_when_listed_and_yields_otherwise() {
        let listed = vec![
            model_row("muse-spark-1.2"),
            model_row(TITLE_MODEL_ID),
            model_row("muse-spark-1.3-contributor"),
        ];
        assert_eq!(pick_title_model(&listed), Some(TITLE_MODEL_ID.to_owned()));
        assert_eq!(pick_title_model(&[model_row("muse-spark-1.2")]), None);
        assert_eq!(pick_title_model(&[]), None);
    }

    #[test]
    fn the_prompt_quotes_the_first_message_and_asks_for_a_title_only() {
        let prompt = title_prompt("Explain how this project is laid out");
        assert!(prompt.contains("Explain how this project is laid out"));
        assert!(prompt.contains("3 to 6 words"));
        assert!(prompt.contains("only the title"));
    }

    #[test]
    fn the_prompt_bounds_a_rambling_first_message() {
        let prompt = title_prompt(&"word ".repeat(10_000));
        assert!(prompt.chars().count() <= TITLE_PROMPT_CHARS + 300);
    }

    #[test]
    fn titles_arrive_unquoted_uncapped_and_single_line() {
        assert_eq!(clean_title("\"Tighten address validation\"\n"), Some("Tighten address validation".into()));
        assert_eq!(clean_title("Title: Fix the header overlap."), Some("Fix the header overlap".into()));
        assert_eq!(clean_title("  \n  "), None);
        assert_eq!(clean_title("…"), Some("…".into()));
    }

    #[test]
    fn an_overlong_title_is_cut_the_rows_own_way() {
        let title = clean_title(&"word ".repeat(100)).expect("a long reply still titles");
        assert_eq!(title, crate::sidebar::one_line(&"word ".repeat(100)));
    }

    #[test]
    fn the_harvest_reads_the_newest_assistant_text() {
        let read = read_with(["queued reply", "Tighten validation"]);
        assert_eq!(harvest_title_text(&read), Some("Tighten validation".into()));
    }

    #[test]
    fn the_harvest_skips_user_text_and_blank_replies() {
        let read = read_with(["   "]);
        assert_eq!(harvest_title_text(&read), None);
        let empty = read_with([]);
        assert_eq!(harvest_title_text(&empty), None);
    }

    fn model_row(id: &str) -> muse_client::schema::ModelCatalogEntry {
        serde_json::from_value(serde_json::json!({
            "modelId": id,
            "displayLabel": id,
            "isActive": false,
            "isDefault": false,
            "providerId": "meta",
        }))
        .expect("a model row deserialises")
    }

    fn agent_item(text: &str) -> serde_json::Value {
        serde_json::json!({
            "itemId": "i",
            "kind": "agentMessage",
            "revision": 1,
            "status": "completed",
            "text": text,
        })
    }

    fn read_with(texts: impl IntoIterator<Item = &'static str>) -> muse_client::schema::SessionReadResult {
        let items: Vec<serde_json::Value> = texts.into_iter().map(agent_item).collect();
        serde_json::from_value(serde_json::json!({
            "history": { "items": items, "mode": "inline" },
            "pendingRequests": [],
            "session": {
                "sessionId": "side",
                "path": "",
                "createdAt": "2026-09-16T00:00:00Z",
                "updatedAt": "2026-09-16T00:00:00Z",
                "turnCount": 1,
                "status": "idle",
            },
            "viewCursor": "c",
        }))
        .expect("a read result deserialises")
    }
}
