//! Baaz's saved copy of Muse's `session/list`, so a launch (and every
//! finished turn) asks only for what changed.
//!
//! Muse 1.4.11 filters `session/list` by `updatedAfter` (probed 2026-10-21,
//! docs/51 §8): strictly later `updatedAt`, microsecond precision, paged
//! like the full listing. A full listing over 2,661 sessions is 27 serial
//! pages of 200, 4.0–4.2 s at boot; the filtered one is one page in
//! 130–360 ms. So the listing remembers its rows, asks for rows newer than
//! the newest `updatedAt` it holds (less an overlap, see [`OVERLAP`]) and
//! merges them in by id.
//!
//! What a filtered listing cannot see is a session that left Muse: deleted
//! rows never come back as "newer". A full listing still runs when the copy
//! is missing, unreadable, from another Muse data dir or older than
//! [`FULL_EVERY`], and whenever a filtered listing fails.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// What the listing needs from a row. Generic so this module stays off the
/// wire crate (the provider seam's ratchet, `tests/seam_ratchet.rs`): the
/// caller implements it for Muse's `Session` and supplies the page call.
pub trait Row: Clone + PartialEq + Serialize + DeserializeOwned {
    /// The session id.
    fn id(&self) -> &str;
    /// `updatedAt`, RFC 3339 — what `updatedAfter` filters on.
    fn updated_at(&self) -> &str;
    /// The latest-activity stamp the server orders by (RFC 3339), or the
    /// creation time when there is no activity yet.
    fn activity_at(&self) -> &str;
    /// Drop what the sidebar never reads before the row is held or saved.
    /// Muse lists whole first prompts and titles: 128 MB each over the
    /// owner's 2,661 sessions (relay briefs run to megabytes), when a row
    /// label reads 80 characters of them.
    fn compact(&mut self);
}

/// One Muse row as Baaz holds and saves it: the seam's
/// [`provider::SessionSummary`] fields the sidebar reads, under the wire's
/// own JSON keys, so a copy saved before the listing went through the seam
/// reads back unchanged (the keys it does not name are ignored).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListedRow {
    /// The session id.
    pub session_id: String,
    /// The name someone gave it in Muse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Muse's own title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The first prompt, as a preview.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_user_prompt: Option<String>,
    /// Where it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
    /// The workspace's branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Created, RFC 3339.
    pub created_at: String,
    /// Content last moved, RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<String>,
    /// Record last touched, RFC 3339.
    pub updated_at: String,
    /// `running` while a turn runs; anything else is not running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Exchanges so far.
    #[serde(default)]
    pub turn_count: u64,
    /// What waits on the person, by name ([`provider::facts::Attention`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<Vec<String>>,
}

impl ListedRow {
    /// A turn is running.
    pub fn running(&self) -> bool {
        self.status.as_deref() == Some("running")
    }

    /// What waits on the person.
    pub fn attention(&self) -> Vec<provider::facts::Attention> {
        self.attention.iter().flatten().map(|name| provider::facts::Attention::parse(name)).collect()
    }
}

impl From<provider::SessionSummary> for ListedRow {
    fn from(row: provider::SessionSummary) -> Self {
        Self {
            session_id: row.session_id,
            name: row.name,
            title: row.title,
            first_user_prompt: row.first_prompt,
            workspace_root: row.workspace,
            branch: row.branch,
            created_at: row.created_at,
            last_activity_at: row.last_activity_at,
            updated_at: row.updated_at,
            status: Some(if row.running { "running" } else { "idle" }.to_owned()),
            turn_count: row.turns,
            attention: (!row.attention.is_empty())
                .then(|| row.attention.iter().map(|flag| flag.name().to_owned()).collect()),
        }
    }
}

impl Row for ListedRow {
    fn id(&self) -> &str {
        &self.session_id
    }
    fn updated_at(&self) -> &str {
        &self.updated_at
    }
    fn activity_at(&self) -> &str {
        self.last_activity_at.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(&self.created_at)
    }
    fn compact(&mut self) {
        // The join reads these for an 80-character label and a prompt-prefix
        // side marker; nothing in Baaz reads past the first 1,000.
        for text in [&mut self.title, &mut self.first_user_prompt, &mut self.name].into_iter().flatten() {
            keep_prefix(text);
        }
    }
}

/// How much of a free-text field a held row keeps: what the index read keeps
/// too (`substr(…, 1, 1000)`, [`crate::index`]), far past the 80-character
/// label.
pub const TEXT_KEEP: usize = 1000;

/// `text` cut to its first [`TEXT_KEEP`] characters.
pub fn keep_prefix(text: &mut String) {
    if let Some((at, _)) = text.char_indices().nth(TEXT_KEEP) {
        text.truncate(at);
    }
}

/// One page of a listing, as the caller's wire call returns it.
pub struct Page<R> {
    /// The rows.
    pub rows: Vec<R>,
    /// The next page's cursor; `None` on the last page.
    pub next: Option<String>,
}

/// One page request: the cursor from the previous page (`None` = first) and
/// the `updatedAfter` filter (`None` = everything).
pub struct Ask<'a> {
    /// Page cursor.
    pub cursor: Option<String>,
    /// Page size.
    pub limit: u32,
    /// Only rows with a strictly later `updatedAt`.
    pub updated_after: Option<&'a str>,
}

/// Bumped when the saved shape changes; an older file is a full listing.
const VERSION: u32 = 1;

/// How old the last full listing may be before the next one runs anyway:
/// the only way a session deleted outside Baaz leaves the sidebar.
pub const FULL_EVERY: chrono::Duration = chrono::Duration::hours(24);

/// How far before the newest `updatedAt` held the filtered listing starts.
/// A row stamped just before another but written after it would otherwise
/// fall between two listings; re-reading a few seconds of rows costs
/// nothing, since the merge is by id.
pub const OVERLAP: chrono::Duration = chrono::Duration::seconds(5);

/// The server's page maximum.
const PAGE: u32 = 200;

/// One saved listing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(bound = "R: Row")]
pub struct ListCache<R> {
    /// [`VERSION`] when written.
    pub version: u32,
    /// The Muse index this listing came from ([`crate::index::index_path`]):
    /// a different `XDG_DATA_HOME` is a different set of sessions.
    pub source: String,
    /// When the last full listing finished, epoch milliseconds.
    pub full_at_ms: i64,
    /// Every row, in the server's order (latest activity first).
    pub sessions: Vec<R>,
}

impl<R> Default for ListCache<R> {
    fn default() -> Self {
        Self { version: VERSION, source: String::new(), full_at_ms: 0, sessions: Vec::new() }
    }
}

/// How a listing was taken, for the boot trace and the tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Every page, from the first.
    Full,
    /// Only rows newer than the saved copy's newest, merged into it.
    Newer,
}

/// What one listing produced.
#[derive(Debug)]
pub struct Listing<R> {
    /// The whole list after the merge.
    pub sessions: Vec<R>,
    /// How it was taken.
    pub mode: Mode,
    /// Pages asked for.
    pub pages: usize,
    /// Rows the server returned (all of them for [`Mode::Full`]).
    pub fetched: usize,
    /// The copy to save, or `None` when nothing in it changed.
    pub save: Option<ListCache<R>>,
}

/// `session-list-cache.json` in Baaz's state dir.
pub fn cache_path() -> PathBuf {
    crate::store::support_dir().join("session-list-cache.json")
}

/// The saved copy, or `None` (no file, or one this build cannot read).
pub fn load<R: Row>(path: &Path) -> Option<ListCache<R>> {
    let bytes = std::fs::read(path).ok()?;
    let cache: ListCache<R> = serde_json::from_slice(&bytes).ok()?;
    (cache.version == VERSION).then_some(cache)
}

/// Save a copy. Blocking; call it off the UI thread.
pub fn save<R: Row>(path: &Path, cache: &ListCache<R>) {
    if let Ok(bytes) = serde_json::to_vec(cache) {
        let _ = crate::store::write_atomic(path, &bytes);
    }
}

fn parse(stamp: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(stamp.trim()).ok().map(|t| t.with_timezone(&Utc))
}

/// The newest `updatedAt` among `sessions`, parsed: the strings carry a
/// varying number of fractional digits, so they do not compare as text.
pub fn newest_updated<R: Row>(sessions: &[R]) -> Option<DateTime<Utc>> {
    sessions.iter().filter_map(|s| parse(s.updated_at())).max()
}

/// The `updatedAfter` a filtered listing sends for `cache`: its newest stamp
/// less [`OVERLAP`]. `None` when the copy holds no stamp at all.
pub fn cut<R: Row>(cache: &ListCache<R>) -> Option<String> {
    newest_updated(&cache.sessions)
        .map(|newest| (newest - OVERLAP).to_rfc3339_opts(chrono::SecondsFormat::Micros, true))
}

/// Whether `cache` can serve a filtered listing at `now_ms` for `source`.
pub fn usable<R: Row>(cache: &ListCache<R>, source: &str, now_ms: i64) -> bool {
    cache.version == VERSION
        && cache.source == source
        && now_ms.saturating_sub(cache.full_at_ms) < FULL_EVERY.num_milliseconds()
        && cut(cache).is_some()
}

/// Merge `newer` into `cached` by id (a newer row replaces its old copy) and
/// order the result like the server: latest activity first, ties by id.
pub fn merge<R: Row>(cached: &[R], newer: Vec<R>) -> Vec<R> {
    let mut by_id: HashMap<String, R> = cached.iter().map(|s| (s.id().to_owned(), s.clone())).collect();
    for session in newer {
        by_id.insert(session.id().to_owned(), session);
    }
    let mut out: Vec<R> = by_id.into_values().collect();
    out.sort_by(|a, b| {
        parse(b.activity_at()).cmp(&parse(a.activity_at())).then_with(|| a.id().cmp(b.id()))
    });
    out
}

/// The page call a listing makes.
pub type PageCall<'a, R, E> = dyn FnMut(Ask<'_>) -> Result<Page<R>, E> + 'a;

/// Follow `session/list` pages from the first until the server says there
/// are no more.
fn pages<R: Row, E>(
    list: &mut PageCall<'_, R, E>,
    updated_after: Option<&str>,
    on_page: &mut dyn FnMut(usize, usize, std::time::Duration),
) -> Result<(Vec<R>, usize), E> {
    let mut sessions = Vec::new();
    let mut cursor: Option<String> = None;
    let mut count = 0usize;
    loop {
        let at = std::time::Instant::now();
        let page = list(Ask { cursor, limit: PAGE, updated_after })?;
        count += 1;
        on_page(count, page.rows.len(), at.elapsed());
        sessions.extend(page.rows.into_iter().map(|mut row| {
            row.compact();
            row
        }));
        cursor = page.next;
        if cursor.is_none() {
            return Ok((sessions, count));
        }
    }
}

/// One listing: filtered against `cached` when it is [`usable`], else (or
/// when the filtered listing fails) every page. Blocking; runs on the wire
/// task.
pub fn list<R: Row, E: std::fmt::Display>(
    cached: Option<&ListCache<R>>,
    source: &str,
    now_ms: i64,
    list: &mut PageCall<'_, R, E>,
    on_page: &mut dyn FnMut(usize, usize, std::time::Duration),
) -> Result<Listing<R>, E> {
    if let Some(cache) = cached.filter(|cache| usable(cache, source, now_ms)) {
        match pages(list, cut(cache).as_deref(), on_page) {
            Ok((newer, count)) => {
                let fetched = newer.len();
                let sessions = merge(&cache.sessions, newer);
                let save = (sessions != cache.sessions).then(|| ListCache {
                    version: VERSION,
                    source: source.to_owned(),
                    full_at_ms: cache.full_at_ms,
                    sessions: sessions.clone(),
                });
                return Ok(Listing { sessions, mode: Mode::Newer, pages: count, fetched, save });
            }
            Err(error) => {
                crate::baaz_log!("session/list since the saved copy failed, listing everything: {error}");
            }
        }
    }
    let (sessions, count) = pages(list, None, on_page)?;
    let save = Some(ListCache {
        version: VERSION,
        source: source.to_owned(),
        full_at_ms: now_ms,
        sessions: sessions.clone(),
    });
    Ok(Listing { fetched: sessions.len(), sessions, mode: Mode::Full, pages: count, save })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A copy saved before the listing went through the seam holds Muse's
    /// whole wire rows; it reads back as the neutral row, keys it does not
    /// name ignored, running and attention intact — so the first launch on
    /// this build asks for newer rows only, not 27 pages.
    #[test]
    fn a_copy_saved_from_wire_rows_reads_back_as_neutral_rows() {
        let saved = serde_json::json!({
            "version": VERSION,
            "source": "/muse/index.db",
            "full_at_ms": 5,
            "sessions": [{
                "sessionId": "s1", "createdAt": "2026-10-01T00:00:00Z", "updatedAt": "2026-10-20T00:00:00Z",
                "lastActivityAt": "2026-10-19T00:00:00Z", "path": "/x.jsonl", "status": "running",
                "turnCount": 4, "title": "Wire title", "generatedName": "ignored", "branch": "main",
                "attention": ["approvalPending", "somethingNew"], "activeTurnId": null, "workspaceRoot": "/w",
            }],
        });
        let copy: ListCache<ListedRow> = serde_json::from_value(saved).expect("an old copy reads");
        let row = &copy.sessions[0];
        assert!(row.running());
        assert_eq!(row.turn_count, 4);
        assert_eq!(row.branch.as_deref(), Some("main"));
        assert_eq!(
            row.attention(),
            vec![provider::facts::Attention::ApprovalPending, provider::facts::Attention::Other("somethingNew".into())]
        );
        assert_eq!(row.activity_at(), "2026-10-19T00:00:00Z");
    }

    #[test]
    fn a_seam_row_becomes_the_held_row() {
        let row = ListedRow::from(provider::SessionSummary {
            session_id: "s2".into(),
            running: true,
            turns: 2,
            attention: vec![provider::facts::Attention::InputPending],
            created_at: "c".into(),
            updated_at: "u".into(),
            ..Default::default()
        });
        assert!(row.running());
        assert_eq!(row.attention(), vec![provider::facts::Attention::InputPending]);
        assert_eq!(row.updated_at(), "u");
        assert_eq!(row.activity_at(), "c", "no activity yet: the creation time");
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct Session {
        session_id: String,
        updated_at: String,
        last_activity_at: String,
        turn_count: u64,
    }

    impl Row for Session {
        fn id(&self) -> &str {
            &self.session_id
        }
        fn updated_at(&self) -> &str {
            &self.updated_at
        }
        fn activity_at(&self) -> &str {
            &self.last_activity_at
        }
        fn compact(&mut self) {
            keep_prefix(&mut self.session_id);
        }
    }

    fn row(id: &str, updated: &str, activity: &str) -> Session {
        Session { session_id: id.into(), updated_at: updated.into(), last_activity_at: activity.into(), turn_count: 1 }
    }

    /// One recorded page request.
    #[derive(Debug)]
    struct Call {
        updated_after: Option<String>,
    }

    /// A scripted server: the full list in its order, filtered and paged the
    /// way muse 1.4.11 does it (strictly later `updatedAt`).
    struct Server {
        rows: Vec<Session>,
        calls: Vec<Call>,
        page: usize,
        fail_filtered: bool,
    }

    impl Server {
        fn call(&mut self, params: Ask<'_>) -> Result<Page<Session>, String> {
            self.calls.push(Call { updated_after: params.updated_after.map(str::to_owned) });
            if self.fail_filtered && params.updated_after.is_some() {
                return Err("closed".into());
            }
            let after = params.updated_after.map(|s| parse(s).expect("a valid stamp"));
            let rows: Vec<Session> = self
                .rows
                .iter()
                .filter(|s| after.is_none_or(|after| parse(&s.updated_at).expect("stamp") > after))
                .cloned()
                .collect();
            let start: usize = params.cursor.as_deref().map(|c| c.parse().expect("cursor")).unwrap_or(0);
            let end = (start + self.page).min(rows.len());
            Ok(Page { next: (end < rows.len()).then(|| end.to_string()), rows: rows[start..end].to_vec() })
        }
    }

    fn server(rows: Vec<Session>) -> Server {
        Server { rows, calls: Vec::new(), page: 2, fail_filtered: false }
    }

    fn run(cached: Option<&ListCache<Session>>, server: &mut Server, now_ms: i64) -> Listing<Session> {
        list(cached, "src", now_ms, &mut |p| server.call(p), &mut |_, _, _| {}).expect("a listing")
    }

    const NOW: i64 = 1_000_000_000_000;

    fn three() -> Vec<Session> {
        vec![
            row("c", "2026-10-21T10:00:00.5Z", "2026-10-21T10:00:00Z"),
            row("b", "2026-10-20T10:00:00Z", "2026-10-20T10:00:00Z"),
            row("a", "2026-10-19T10:00:00.123456Z", "2026-10-19T10:00:00Z"),
        ]
    }

    #[test]
    fn no_copy_lists_every_page_and_saves_it() {
        let mut s = server(three());
        let listing = run(None, &mut s, NOW);
        assert_eq!(listing.mode, Mode::Full);
        assert_eq!(listing.pages, 2, "three rows at two a page");
        assert!(s.calls.iter().all(|c| c.updated_after.is_none()));
        let save = listing.save.expect("a full listing is always saved");
        assert_eq!(save.full_at_ms, NOW);
        assert_eq!(save.sessions, three());
    }

    #[test]
    fn a_current_copy_asks_only_for_newer_rows_and_merges_them() {
        let mut s = server(three());
        let copy = run(None, &mut s, NOW).save.expect("saved");
        // One session moved on, one is new, the rest did not.
        let mut moved = row("a", "2026-10-21T11:00:00Z", "2026-10-21T11:00:00Z");
        moved.turn_count = 2;
        let fresh = row("d", "2026-10-21T12:00:00Z", "2026-10-21T12:00:00Z");
        s.rows = vec![fresh.clone(), moved.clone(), three()[0].clone(), three()[1].clone()];
        s.calls.clear();
        let listing = run(Some(&copy), &mut s, NOW + 60_000);
        assert_eq!(listing.mode, Mode::Newer);
        assert_eq!(s.calls.len(), 2, "three newer rows, paged two at a time");
        assert!(s.calls.iter().all(|c| c.updated_after.is_some()), "every page keeps the filter");
        // The newest held stamp was c's 10:00:00.5, less the overlap.
        assert_eq!(s.calls[0].updated_after.as_deref(), Some("2026-10-21T09:59:55.500000Z"));
        // c falls inside the overlap and comes back; b does not.
        assert_eq!(listing.fetched, 3);
        let ids: Vec<&str> = listing.sessions.iter().map(|x| x.session_id.as_str()).collect();
        assert_eq!(ids, ["d", "a", "c", "b"], "merged, latest activity first");
        assert_eq!(listing.sessions[1].turn_count, 2, "a moved row replaces its old copy");
        let save = listing.save.expect("the merge changed the list");
        assert_eq!(save.full_at_ms, NOW, "a filtered listing keeps the full listing's time");
    }

    #[test]
    fn nothing_newer_saves_nothing() {
        let mut s = server(three());
        let copy = run(None, &mut s, NOW).save.expect("saved");
        let listing = run(Some(&copy), &mut s, NOW + 1);
        assert_eq!(listing.mode, Mode::Newer);
        assert_eq!(listing.sessions, three());
        assert!(listing.save.is_none(), "an unchanged list is not rewritten");
    }

    #[test]
    fn an_old_copy_or_another_muse_lists_everything() {
        let mut s = server(three());
        let copy = run(None, &mut s, NOW).save.expect("saved");
        let day = FULL_EVERY.num_milliseconds();
        assert_eq!(run(Some(&copy), &mut s, NOW + day).mode, Mode::Full, "a day old");
        assert_eq!(run(Some(&copy), &mut s, NOW + day - 1).mode, Mode::Newer);
        let mut elsewhere = copy.clone();
        elsewhere.source = "another muse".into();
        assert_eq!(run(Some(&elsewhere), &mut s, NOW).mode, Mode::Full);
        let mut empty = copy;
        empty.sessions.clear();
        assert_eq!(run(Some(&empty), &mut s, NOW).mode, Mode::Full, "no stamp to start from");
    }

    #[test]
    fn a_failed_filtered_listing_falls_back_to_every_page() {
        let mut s = server(three());
        let copy = run(None, &mut s, NOW).save.expect("saved");
        s.fail_filtered = true;
        let listing = run(Some(&copy), &mut s, NOW + 1);
        assert_eq!(listing.mode, Mode::Full);
        assert_eq!(listing.sessions, three());
    }

    #[test]
    fn a_session_that_left_muse_goes_at_the_next_full_listing() {
        let mut s = server(three());
        let copy = run(None, &mut s, NOW).save.expect("saved");
        s.rows.remove(1);
        let newer = run(Some(&copy), &mut s, NOW + 1);
        assert_eq!(newer.sessions.len(), 3, "a filtered listing cannot see a deletion");
        let full = run(Some(&copy), &mut s, NOW + FULL_EVERY.num_milliseconds());
        assert_eq!(full.sessions.len(), 2);
    }

    #[test]
    fn rows_are_compacted_as_they_arrive() {
        let long = "é".repeat(TEXT_KEEP + 5);
        let mut s = server(vec![row(&long, "2026-10-21T10:00:00Z", "2026-10-21T10:00:00Z")]);
        let listing = run(None, &mut s, NOW);
        assert_eq!(listing.sessions[0].session_id.chars().count(), TEXT_KEEP, "cut on a char boundary");
        let mut short = String::from("short");
        keep_prefix(&mut short);
        assert_eq!(short, "short");
    }

    #[test]
    fn the_copy_round_trips_and_a_foreign_version_is_no_copy() {
        let dir = std::env::temp_dir().join(format!("baaz-list-cache-{}", std::process::id()));
        let path = dir.join("session-list-cache.json");
        let mut s = server(three());
        let copy = run(None, &mut s, NOW).save.expect("saved");
        save(&path, &copy);
        assert_eq!(load(&path), Some(copy.clone()));
        let mut old = copy;
        old.version = VERSION + 1;
        save(&path, &old);
        assert_eq!(load::<Session>(&path), None);
        std::fs::write(&path, b"not json").expect("write");
        assert_eq!(load::<Session>(&path), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
