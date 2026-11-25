//! `session_search` and `session_read` (docs/85 12.3): the agent finding
//! past work.
//!
//! **Search.** The agent passes 3–5 phrasings of what it is looking for;
//! each runs against `search.db` (stemmed words and name parts, 11.3) and
//! the lists merge by reciprocal rank, so a session several phrasings find
//! rises. Each result names its title, date, provider, project, digest and
//! the snippet around the match. The search defaults to the current
//! project; `scope: all` widens it. Side sessions and browser chats are not
//! results.
//!
//! **Read.** The turns around the hits in one session, from Baaz's own
//! transcript store (11.1) — which keeps the turns from before a
//! compaction, so a session can read what it compacted away. Secrets are
//! scrubbed ([`crate::scrub`]) and the answer is capped.

use std::collections::HashMap;

use rusqlite::Connection;

use crate::search::SessionHit;

/// Results a search returns at most.
pub const SEARCH_LIMIT: usize = 8;

/// Characters a read returns at most.
pub const READ_CHARS: usize = 8000;

/// Turns kept on each side of a hit in a read.
pub const READ_CONTEXT: usize = 2;

/// One search result.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Found {
    /// The session's id (what `session_read` takes).
    pub session: String,
    /// Its title.
    pub title: String,
    /// The provider it ran on.
    pub provider: String,
    /// Its project folder's name.
    pub project: String,
    /// The day it was last active (`YYYY-MM-DD`).
    pub date: String,
    /// Its digest, when one was written (12.4).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub digest: String,
    /// The text around the match.
    pub snippet: String,
}

/// What baaz.db knows of a session for a result.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// Provider key.
    pub provider: String,
    /// Best title.
    pub title: String,
    /// Its folder.
    pub workspace: Option<String>,
    /// Last active (ms).
    pub updated_ms: Option<i64>,
    /// The digest.
    pub digest: String,
    /// A side session or a browser chat: never a result.
    pub excluded: bool,
}

/// Merge each phrasing's hits by reciprocal rank (k = 10): a session found
/// by several phrasings, or high in one, comes first. Ties keep the order
/// the first phrasing gave.
pub fn merge(lists: &[Vec<SessionHit>]) -> Vec<(SessionHit, f64)> {
    let mut scores: HashMap<String, (f64, usize, SessionHit)> = HashMap::new();
    let mut order = 0;
    for list in lists {
        for (rank, hit) in list.iter().enumerate() {
            let entry = scores.entry(hit.session_id.clone()).or_insert_with(|| {
                order += 1;
                (0.0, order, hit.clone())
            });
            entry.0 += 1.0 / (10.0 + rank as f64);
        }
    }
    let mut merged: Vec<(f64, usize, SessionHit)> = scores.into_values().collect();
    merged.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    merged.into_iter().map(|(score, _, hit)| (hit, score)).collect()
}

/// The search: phrasings run with `run`, merged, scoped to `project_root`
/// unless it is `None`, each result filled from `facts`.
pub fn search(
    phrasings: &[String],
    project_root: Option<&str>,
    run: &dyn Fn(&str) -> Vec<SessionHit>,
    facts: &dyn Fn(&str) -> Option<Facts>,
) -> Result<Vec<Found>, String> {
    let phrasings: Vec<&str> = phrasings.iter().map(|q| q.trim()).filter(|q| !q.is_empty()).collect();
    if phrasings.is_empty() {
        return Err("give 3–5 phrasings of what you are looking for in `queries`".into());
    }
    let lists: Vec<Vec<SessionHit>> = phrasings.iter().map(|q| run(q)).collect();
    let mut found = Vec::new();
    for (hit, _) in merge(&lists) {
        let Some(facts) = facts(&hit.session_id) else { continue };
        if facts.excluded {
            continue;
        }
        let workspace = facts.workspace.clone().or(hit.workspace.clone());
        if let Some(root) = project_root {
            if !workspace.as_deref().is_some_and(|w| w == root || w.starts_with(&format!("{root}/"))) {
                continue;
            }
        }
        found.push(Found {
            session: hit.session_id.clone(),
            title: if facts.title.is_empty() { hit.label.clone() } else { facts.title.clone() },
            provider: facts.provider.clone(),
            project: workspace
                .as_deref()
                .and_then(|w| std::path::Path::new(w).file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            date: facts.updated_ms.map(day).unwrap_or_default(),
            digest: facts.digest.clone(),
            snippet: crate::scrub::scrub(&hit.snippet),
        });
        if found.len() == SEARCH_LIMIT {
            break;
        }
    }
    Ok(found)
}

fn day(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).map(|at| at.format("%Y-%m-%d").to_string()).unwrap_or_default()
}

/// A session's facts from baaz.db, by its provider-side id.
pub fn facts_from_db(conn: &Connection, session: &str) -> Option<Facts> {
    conn.query_row(
        "SELECT provider_key, COALESCE(name, handoff_title, generated_title, provider_title, derived_title, first_prompt, ''),
                workspace_path, updated_at, COALESCE(last_summary, ''), side_session OR browser_chat
         FROM session WHERE provider_session_id = ?1 ORDER BY updated_at DESC LIMIT 1",
        [session],
        |row| {
            Ok(Facts {
                provider: row.get(0)?,
                title: row.get(1)?,
                workspace: row.get(2)?,
                updated_ms: row.get(3)?,
                digest: row.get(4)?,
                excluded: row.get::<_, i64>(5)? != 0,
            })
        },
    )
    .ok()
}

/// One turn as plain text: who spoke, and what.
pub fn turn_text(turn: &aui_protocol::Turn) -> String {
    use aui_protocol::{Block, Turn};
    match turn {
        Turn::User { text, .. } => format!("User: {text}"),
        Turn::Assistant { blocks, .. } => {
            let mut out = String::from("Assistant:");
            for block in blocks {
                match block {
                    Block::Text { text, .. } => {
                        out.push(' ');
                        out.push_str(text);
                    }
                    Block::ToolCall { verb, target, .. } => out.push_str(&format!(" [{verb} {target}]")),
                    _ => {}
                }
            }
            out
        }
    }
}

/// The turns of a session around the hits of `query` (every word of it,
/// case-blind), [`READ_CONTEXT`] each side; the last turns when `query`
/// finds nothing or is empty. Scrubbed, capped at [`READ_CHARS`].
pub fn read(turns: &[aui_protocol::Turn], query: Option<&str>) -> String {
    let texts: Vec<String> = turns.iter().map(turn_text).collect();
    let words: Vec<String> = query.unwrap_or("").split_whitespace().map(str::to_lowercase).collect();
    let mut keep = vec![false; texts.len()];
    if !words.is_empty() {
        for (index, text) in texts.iter().enumerate() {
            let lower = text.to_lowercase();
            if words.iter().all(|word| lower.contains(word.as_str())) {
                let last = (index + READ_CONTEXT).min(texts.len().saturating_sub(1));
                keep[index.saturating_sub(READ_CONTEXT)..=last].iter_mut().for_each(|flag| *flag = true);
            }
        }
    }
    if !keep.contains(&true) {
        let from = texts.len().saturating_sub(2 * READ_CONTEXT + 2);
        for flag in keep.iter_mut().skip(from) {
            *flag = true;
        }
    }
    let mut out = String::new();
    let mut gap = false;
    for (index, text) in texts.iter().enumerate() {
        if !keep[index] {
            gap = true;
            continue;
        }
        if gap && !out.is_empty() {
            out.push_str("…\n");
        }
        gap = false;
        out.push_str(&format!("[turn {}] {}\n", index + 1, text.trim()));
    }
    let mut out = crate::scrub::scrub(&out);
    if out.chars().count() > READ_CHARS {
        out = out.chars().take(READ_CHARS).collect::<String>() + "\n… (cut: read again with a narrower query)";
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use aui_protocol::{Block, Turn, TurnMeta};

    fn hit(id: &str, workspace: &str) -> SessionHit {
        SessionHit { session_id: id.into(), label: format!("label {id}"), snippet: format!("snippet of {id}"), workspace: Some(workspace.into()) }
    }

    #[test]
    fn several_phrasings_merge_by_rank() {
        let lists = vec![
            vec![hit("a", "/w"), hit("b", "/w"), hit("c", "/w")],
            vec![hit("c", "/w"), hit("b", "/w")],
            vec![hit("b", "/w")],
        ];
        let order: Vec<String> = merge(&lists).into_iter().map(|(hit, _)| hit.session_id).collect();
        assert_eq!(order, ["b", "c", "a"], "found by three phrasings beats found first by one");
    }

    fn user(text: &str) -> Turn {
        Turn::User { id: "u".into(), text: text.into(), attachments: Vec::new(), mentions: Vec::new(), timestamp: None }
    }

    fn assistant(text: &str) -> Turn {
        Turn::Assistant { id: "a".into(), blocks: vec![Block::Text { text: text.into(), streaming: false }], meta: TurnMeta::default(), timestamp: None }
    }

    /// The class-10 recall scenario (docs/84 eval class 10): a decision made
    /// in an earlier session of the project is found by a later session
    /// that asks in other words, and read back around the hit.
    #[test]
    fn a_decision_from_an_earlier_session_is_found_and_read_back() {
        let sessions: HashMap<&str, (Facts, Vec<Turn>, &str)> = HashMap::from([
            ("old", (Facts { provider: "claude-code".into(), title: "Postcode validation".into(), workspace: Some("/repo/web".into()), updated_ms: Some(1_793_000_000_000), ..Facts::default() },
                vec![user("How should GB postcodes be checked?"), assistant("We decided: GB postcodes use the official regex, no space normalisation, because the API rejects spaces."), user("ok ship it"), assistant("Shipped.")],
                "GB postcodes use the official regex, no space normalisation")),
            ("other", (Facts { provider: "codex".into(), title: "Other repo".into(), workspace: Some("/elsewhere".into()), ..Facts::default() }, vec![], "postcode regex elsewhere")),
            ("side", (Facts { provider: "muse".into(), title: "title side".into(), workspace: Some("/repo".into()), excluded: true, ..Facts::default() }, vec![], "postcode")),
        ]);
        let run = |query: &str| -> Vec<SessionHit> {
            let words: Vec<String> = query.to_lowercase().split_whitespace().map(str::to_owned).collect();
            let mut hits: Vec<SessionHit> = sessions
                .iter()
                .filter(|(_, (_, _, body))| words.iter().any(|w| body.to_lowercase().contains(w.as_str())))
                .map(|(id, (facts, _, body))| SessionHit { session_id: (*id).into(), label: facts.title.clone(), snippet: (*body).into(), workspace: facts.workspace.clone() })
                .collect();
            hits.sort_by(|a, b| a.session_id.cmp(&b.session_id));
            hits
        };
        let facts = |id: &str| sessions.get(id).map(|(facts, _, _)| facts.clone());
        let phrasings = ["how we validate UK postcodes", "postcode regex decision", "GB post code spaces"].map(String::from);
        let found = search(&phrasings, Some("/repo"), &run, &facts).unwrap();
        assert_eq!(found.len(), 1, "this project only, never a side session: {found:?}");
        assert_eq!(found[0].session, "old");
        assert_eq!(found[0].provider, "claude-code");
        assert_eq!(found[0].project, "web");
        assert_eq!(found[0].date, "2026-10-26");
        let all = search(&phrasings, None, &run, &facts).unwrap();
        assert!(all.iter().any(|f| f.session == "other"), "scope all widens");
        let text = read(&sessions["old"].1, Some("regex"));
        assert!(text.contains("We decided: GB postcodes use the official regex"), "{text}");
        assert!(text.contains("[turn 1] User: How should GB postcodes be checked?"), "the question around it");
        assert!(search(&[String::new()], None, &run, &facts).is_err(), "phrasings are required");
    }

    #[test]
    fn a_read_is_scrubbed_and_capped() {
        let mut turns = vec![user("my key is sk-ant-api03-AbCdEfGhIjKlMnOpQrStUv please")];
        turns.extend((0..400).map(|i| assistant(&format!("line {i} {}", "x".repeat(100)))));
        let text = read(&turns[..1], None);
        assert!(!text.contains("sk-ant-api03") && text.contains("[secret removed]"), "{text}");
        let mut many = turns.clone();
        many.iter_mut().skip(1).for_each(|turn| if let Turn::Assistant { blocks, .. } = turn { blocks[0] = Block::Text { text: format!("needle {}", "y".repeat(100)), streaming: false } });
        let long = read(&many, Some("needle"));
        assert!(long.chars().count() <= READ_CHARS + 60, "capped");
        assert!(long.ends_with("read again with a narrower query)"));
    }
}
