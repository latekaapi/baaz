//! R4 `browser.db` (docs/27 §7): the person's browsing history, with the words
//! of pages they stayed on, searched by the Field.
//!
//! One SQLite file at `$BAAZ_STATE_DIR/Browser/browser.db`; captures and
//! tests (`BAAZ_DETERMINISTIC=1`) get an in-memory one, so a probe run never
//! reads or writes the owner's history.
//!
//! What is recorded, and what never is:
//! - only the person's own tabs: a session's (agent's) tabs never reach
//!   history ([`Recorder`] takes the owner);
//! - only `http(s)` pages: `about:`, `chrome:`, `file:`, `data:` never;
//! - never a site on the exclusion list (Settings › Browser › Your data), and
//!   adding a site there forgets what was already kept of it;
//! - page words only after the page stayed shown [`DWELL_MS`] (10 s, docs/27);
//! - history older than [`KEEP_DAYS`] is pruned at open.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};

/// Page words are kept once a page has been shown this long.
pub(crate) const DWELL_MS: u64 = 10_000;
/// History is kept this many days.
pub(crate) const KEEP_DAYS: u64 = 90;
/// The most of a page's words kept for search.
const TEXT_CAP: usize = 64 * 1024;
const DAY_MS: u64 = 86_400_000;

/// `$BAAZ_STATE_DIR/Browser/browser.db`.
pub(crate) fn path() -> PathBuf {
    crate::store::support_dir().join("Browser").join("browser.db")
}

/// One remembered page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Visit {
    pub url: String,
    pub title: String,
    /// Last visit, ms since the epoch.
    pub last_ms: u64,
    pub count: u64,
    /// The line of the page's words that matched, when the match was in its
    /// words rather than its title or address.
    pub snippet: Option<String>,
}

/// The history store. Every call is cheap and synchronous (WAL, a few
/// rows); a failed write is logged and dropped, never surfaced.
pub(crate) struct BrowserDb {
    conn: Connection,
}

/// Whether a URL may enter history at all (scheme only; exclusions are
/// checked against the store).
pub(crate) fn recordable(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

/// A URL's host, lower-cased, without `www.`: what the exclusion list holds.
pub(crate) fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit_once('@').map(|(_, h)| h).unwrap_or(host);
    let host = host.split(':').next().unwrap_or("").to_ascii_lowercase();
    host.strip_prefix("www.").unwrap_or(&host).to_owned()
}

/// Whether `host` falls under the excluded `site` (the site itself or a
/// subdomain of it).
fn host_matches(host: &str, site: &str) -> bool {
    host == site || host.ends_with(&format!(".{site}"))
}

/// The query as FTS5 prefix terms: every word quoted (so `-`, `:` and
/// quotes in it are literal) and prefixed. `None` when nothing is left.
fn fts_query(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{}\"*", term.replace('"', "")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

impl BrowserDb {
    /// The owner's store, or an in-memory one under captures and tests and
    /// when the file cannot be opened (then nothing persists this run).
    pub(crate) fn open() -> Self {
        if crate::clock::deterministic() || cfg!(test) {
            return Self::memory();
        }
        match Self::open_at(&path()) {
            Ok(db) => db,
            Err(error) => {
                crate::baaz_log!("browser.db: cannot open ({error}); history is off this run");
                Self::memory()
            }
        }
    }

    pub(crate) fn memory() -> Self {
        Self::open_at(Path::new(":memory:")).expect("an in-memory browser.db always opens")
    }

    pub(crate) fn open_at(path: &Path) -> Result<Self, rusqlite::Error> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        let conn = Connection::open(path)?;
        let _ = conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()));
        conn.execute_batch(
            "PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);
             CREATE TABLE IF NOT EXISTS visits(
                 url TEXT PRIMARY KEY,
                 title TEXT NOT NULL DEFAULT '',
                 host TEXT NOT NULL DEFAULT '',
                 first_ms INTEGER NOT NULL,
                 last_ms INTEGER NOT NULL,
                 count INTEGER NOT NULL DEFAULT 1,
                 source TEXT NOT NULL DEFAULT 'baaz');
             CREATE INDEX IF NOT EXISTS visits_last ON visits(last_ms DESC);
             CREATE INDEX IF NOT EXISTS visits_host ON visits(host);
             CREATE VIRTUAL TABLE IF NOT EXISTS pages_fts USING fts5(url UNINDEXED, title, body);
             CREATE TABLE IF NOT EXISTS excluded(host TEXT PRIMARY KEY);",
        )?;
        let _ = conn.busy_timeout(std::time::Duration::from_secs(2));
        super::bookmarks::ensure_schema(&conn)?;
        let db = Self { conn };
        db.prune(crate::browser::tabs::now_ms());
        Ok(db)
    }

    /// The connection, for the tables that share the file (bookmarks).
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    fn excluded_host(&self, host: &str) -> bool {
        self.excluded().iter().any(|site| host_matches(host, site))
    }

    /// The sites history never records, sorted.
    pub(crate) fn excluded(&self) -> Vec<String> {
        let Ok(mut stmt) = self.conn.prepare_cached("SELECT host FROM excluded ORDER BY host") else {
            return Vec::new();
        };
        stmt.query_map([], |row| row.get::<_, String>(0))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// Never record `site` again, and forget what is kept of it.
    pub(crate) fn exclude(&self, site: &str) {
        let site = host_of(site);
        if site.is_empty() {
            return;
        }
        let pattern = format!("%.{site}");
        let _ = self.conn.execute("INSERT OR IGNORE INTO excluded(host) VALUES (?1)", params![site]);
        let urls: Vec<String> = self
            .conn
            .prepare("SELECT url FROM visits WHERE host = ?1 OR host LIKE ?2")
            .and_then(|mut stmt| {
                stmt.query_map(params![site, pattern], |row| row.get::<_, String>(0)).map(|rows| rows.flatten().collect())
            })
            .unwrap_or_default();
        for url in urls {
            self.remove(&url);
        }
    }

    /// Record `site` again.
    pub(crate) fn include(&self, site: &str) {
        let _ = self.conn.execute("DELETE FROM excluded WHERE host = ?1", params![host_of(site)]);
    }

    /// A visit to `url`: a new row, or one more visit to a known page.
    /// Returns whether anything was written.
    pub(crate) fn record_visit(&self, url: &str, title: &str, now: u64) -> bool {
        if !recordable(url) {
            return false;
        }
        let host = host_of(url);
        if self.excluded_host(&host) {
            return false;
        }
        let result = self.conn.execute(
            "INSERT INTO visits(url, title, host, first_ms, last_ms, count) VALUES (?1, ?2, ?3, ?4, ?4, 1)
             ON CONFLICT(url) DO UPDATE SET
                 last_ms = excluded.last_ms,
                 count = count + 1,
                 title = CASE WHEN excluded.title = '' THEN title ELSE excluded.title END",
            params![url, title, host, now as i64],
        );
        if let Err(error) = &result {
            crate::baaz_log!("browser.db: visit not recorded ({error})");
        }
        result.is_ok()
    }

    /// A known page's title arrived (or changed) after its visit.
    pub(crate) fn set_title(&self, url: &str, title: &str) {
        if title.is_empty() {
            return;
        }
        let _ = self.conn.execute("UPDATE visits SET title = ?2 WHERE url = ?1", params![url, title]);
    }

    /// Keep a page's words for search (after the dwell). Only a page already
    /// in history: an excluded site never had a row.
    pub(crate) fn set_text(&self, url: &str, title: &str, text: &str) {
        let known: bool = self
            .conn
            .query_row("SELECT 1 FROM visits WHERE url = ?1", params![url], |_| Ok(true))
            .optional()
            .ok()
            .flatten()
            .unwrap_or(false);
        if !known {
            return;
        }
        let mut body: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if body.len() > TEXT_CAP {
            let mut cut = TEXT_CAP;
            while !body.is_char_boundary(cut) {
                cut -= 1;
            }
            body.truncate(cut);
        }
        let _ = self.conn.execute("DELETE FROM pages_fts WHERE url = ?1", params![url]);
        let _ = self
            .conn
            .execute("INSERT INTO pages_fts(url, title, body) VALUES (?1, ?2, ?3)", params![url, title, body]);
    }

    /// Forget one page.
    pub(crate) fn remove(&self, url: &str) {
        let _ = self.conn.execute("DELETE FROM visits WHERE url = ?1", params![url]);
        let _ = self.conn.execute("DELETE FROM pages_fts WHERE url = ?1", params![url]);
    }

    /// Forget everything visited since `since_ms` (0: all history).
    pub(crate) fn clear_since(&self, since_ms: u64) {
        if since_ms == 0 {
            let _ = self.conn.execute_batch("DELETE FROM visits; DELETE FROM pages_fts;");
            return;
        }
        let _ = self.conn.execute(
            "DELETE FROM pages_fts WHERE url IN (SELECT url FROM visits WHERE last_ms >= ?1)",
            params![since_ms as i64],
        );
        let _ = self.conn.execute("DELETE FROM visits WHERE last_ms >= ?1", params![since_ms as i64]);
    }

    /// How many pages history holds.
    pub(crate) fn len(&self) -> u64 {
        self.conn.query_row("SELECT count(*) FROM visits", [], |row| row.get::<_, i64>(0)).unwrap_or(0) as u64
    }

    fn prune(&self, now: u64) {
        let cutoff = now.saturating_sub(KEEP_DAYS * DAY_MS) as i64;
        let _ = self.conn.execute(
            "DELETE FROM pages_fts WHERE url IN (SELECT url FROM visits WHERE last_ms < ?1)",
            params![cutoff],
        );
        let _ = self.conn.execute("DELETE FROM visits WHERE last_ms < ?1", params![cutoff]);
    }

    /// The most recent pages, newest first.
    #[cfg(test)]
    pub(crate) fn recent(&self, limit: usize) -> Vec<Visit> {
        let Ok(mut stmt) =
            self.conn.prepare_cached("SELECT url, title, last_ms, count FROM visits ORDER BY last_ms DESC LIMIT ?1")
        else {
            return Vec::new();
        };
        stmt.query_map(params![limit as i64], visit_row)
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// Pages matching `query`: by title or address first (newest, then most
    /// visited), then pages whose words match, each with the matching line.
    /// At most `limit` rows, no page twice.
    pub(crate) fn search(&self, query: &str, limit: usize) -> Vec<Visit> {
        let q = query.trim();
        if q.is_empty() || limit == 0 {
            return Vec::new();
        }
        let like = format!("%{}%", q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        let mut out: Vec<Visit> = self
            .conn
            .prepare_cached(
                "SELECT url, title, last_ms, count FROM visits
                 WHERE title LIKE ?1 ESCAPE '\\' OR url LIKE ?1 ESCAPE '\\'
                 ORDER BY last_ms DESC LIMIT ?2",
            )
            .and_then(|mut stmt| stmt.query_map(params![like, limit as i64], visit_row).map(|rows| rows.flatten().collect()))
            .unwrap_or_default();
        if out.len() < limit {
            if let Some(fts) = fts_query(q) {
                let words: Vec<Visit> = self
                    .conn
                    .prepare_cached(
                        "SELECT v.url, v.title, v.last_ms, v.count,
                                snippet(pages_fts, 2, '', '', '…', 12)
                         FROM pages_fts JOIN visits v ON v.url = pages_fts.url
                         WHERE pages_fts MATCH ?1 ORDER BY rank LIMIT ?2",
                    )
                    .and_then(|mut stmt| {
                        stmt.query_map(params![fts, limit as i64], |row| {
                            let mut visit = visit_row(row)?;
                            visit.snippet = row.get::<_, String>(4).ok().filter(|s| !s.is_empty());
                            Ok(visit)
                        })
                        .map(|rows| rows.flatten().collect())
                    })
                    .unwrap_or_default();
                for visit in words {
                    if out.len() >= limit {
                        break;
                    }
                    if !out.iter().any(|seen| seen.url == visit.url) {
                        out.push(visit);
                    }
                }
            }
        }
        out
    }
}

fn visit_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Visit> {
    Ok(Visit {
        url: row.get(0)?,
        title: row.get(1)?,
        last_ms: row.get::<_, i64>(2)? as u64,
        count: row.get::<_, i64>(3)? as u64,
        snippet: None,
    })
}

/// A relative word for a visit (`just now`, `5 min ago`, `today`,
/// `yesterday`, `3 days ago`, `last week`, `12 Oct`).
pub(crate) fn ago(then: u64, now: u64) -> String {
    let delta = now.saturating_sub(then);
    match delta {
        d if d < 60_000 => "just now".to_owned(),
        d if d < 3_600_000 => format!("{} min ago", d / 60_000),
        d if d < DAY_MS => "today".to_owned(),
        d if d < 2 * DAY_MS => "yesterday".to_owned(),
        d if d < 7 * DAY_MS => format!("{} days ago", d / DAY_MS),
        d if d < 14 * DAY_MS => "last week".to_owned(),
        d => format!("{} weeks ago", d / (7 * DAY_MS)),
    }
}

/// What each live tab is doing, for history: the URL last recorded and
/// since when it has been shown, so a page is recorded once per visit and
/// its words once per dwell.
#[derive(Default)]
pub(crate) struct Recorder {
    seen: std::collections::HashMap<super::TabId, Seen>,
}

#[derive(Clone, Debug, Default)]
struct Seen {
    url: String,
    title: String,
    /// When this tab started showing `url` in front, if it is in front.
    shown_since: Option<u64>,
    /// Whether the page's words were asked for on this visit.
    text_asked: bool,
}

/// What the recorder wants done after looking at one tab.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Note {
    Nothing,
    /// Ask the page for its words now.
    WantText,
}

impl Recorder {
    /// One tab, as the sync sees it: record a new visit, a late title, and
    /// say when the words are due. `person` is false for a session's tab,
    /// which is never recorded. `shown` is whether it is the page in front.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn observe(
        &mut self,
        db: &BrowserDb,
        id: super::TabId,
        person: bool,
        url: &str,
        title: &str,
        shown: bool,
        now: u64,
    ) -> Note {
        if !person || !recordable(url) {
            self.seen.remove(&id);
            return Note::Nothing;
        }
        let seen = self.seen.entry(id).or_default();
        if seen.url != url {
            *seen = Seen { url: url.to_owned(), title: title.to_owned(), shown_since: None, text_asked: false };
            db.record_visit(url, title, now);
        } else if seen.title != title && !title.is_empty() {
            seen.title = title.to_owned();
            db.set_title(url, title);
        }
        match (shown, seen.shown_since) {
            (false, _) => seen.shown_since = None,
            (true, None) => seen.shown_since = Some(now),
            (true, Some(since)) if !seen.text_asked && now.saturating_sub(since) >= DWELL_MS => {
                // Every dwelt visit refreshes the words: pages change.
                seen.text_asked = true;
                return Note::WantText;
            }
            _ => {}
        }
        Note::Nothing
    }

    /// Drop tabs that no longer exist.
    pub(crate) fn retain(&mut self, live: impl Fn(super::TabId) -> bool) {
        self.seen.retain(|id, _| live(*id));
    }

    /// The URL a tab is recorded at, to check a late answer still belongs.
    pub(crate) fn url_of(&self, id: super::TabId) -> Option<&str> {
        self.seen.get(&id).map(|seen| seen.url.as_str())
    }
}

/// The script that reads a page's words for history.
pub(crate) const PAGE_TEXT_JS: &str = "return (document.body && document.body.innerText || '').slice(0, 200000)";

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_800_000_000_000;

    #[test]
    fn a_visit_is_recorded_once_and_counted() {
        let db = BrowserDb::memory();
        assert!(db.record_visit("https://rust-lang.org/learn", "Learn Rust", T0));
        assert!(db.record_visit("https://rust-lang.org/learn", "", T0 + 5));
        let rows = db.recent(10);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].count, 2);
        assert_eq!(rows[0].title, "Learn Rust", "an empty later title keeps the known one");
        assert_eq!(rows[0].last_ms, T0 + 5);
    }

    #[test]
    fn only_web_pages_enter_history() {
        let db = BrowserDb::memory();
        for url in ["about:blank", "chrome://settings", "file:///tmp/a.html", "data:text/html,hi", ""] {
            assert!(!db.record_visit(url, "x", T0), "{url} must not be recorded");
        }
        assert_eq!(db.len(), 0);
    }

    #[test]
    fn an_excluded_site_is_never_recorded_and_its_past_is_forgotten() {
        let db = BrowserDb::memory();
        db.record_visit("https://mail.bank.example/inbox", "Inbox", T0);
        db.record_visit("https://bank.example/", "Bank", T0);
        db.record_visit("https://other.example/", "Other", T0);
        db.set_text("https://bank.example/", "Bank", "balance statement");
        db.exclude("https://www.bank.example/login");
        assert_eq!(db.excluded(), vec!["bank.example".to_owned()]);
        assert_eq!(db.recent(10).iter().map(|v| v.url.as_str()).collect::<Vec<_>>(), vec!["https://other.example/"]);
        assert!(db.search("balance", 5).is_empty(), "the words went too");
        assert!(!db.record_visit("https://bank.example/again", "x", T0 + 1));
        db.include("bank.example");
        assert!(db.record_visit("https://bank.example/again", "x", T0 + 2));
    }

    #[test]
    fn search_finds_titles_and_addresses_then_words_with_the_line() {
        let db = BrowserDb::memory();
        db.record_visit("https://tokio.rs/tokio/tutorial/spawning", "Tokio tutorial · Spawning", T0);
        db.record_visit("https://without.boats/blog/pin/", "Pin and suffering", T0 + 10);
        db.set_text(
            "https://without.boats/blog/pin/",
            "Pin and suffering",
            "Long text. Self-referential futures in Rust need Pin to stay put. More text follows here.",
        );
        let hits = db.search("tokio", 5);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.is_none(), "a title match has no words line");
        let hits = db.search("rust", 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://without.boats/blog/pin/");
        assert!(hits[0].snippet.as_deref().unwrap_or("").contains("Rust"), "{:?}", hits[0].snippet);
        assert!(db.search("referen", 5).iter().any(|v| v.url.contains("boats")), "prefix match on words");
        assert!(db.search("\"; DROP TABLE visits; --", 5).is_empty(), "quotes are literal");
        assert_eq!(db.len(), 2);
        assert!(db.search("100%", 5).is_empty(), "% is literal");
    }

    #[test]
    fn words_are_kept_only_for_pages_in_history() {
        let db = BrowserDb::memory();
        db.set_text("https://never.example/", "x", "secret words");
        assert!(db.search("secret", 5).is_empty());
    }

    #[test]
    fn old_history_is_pruned_and_clear_forgets() {
        let path = std::env::temp_dir().join(format!("baaz-browserdb-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let db = BrowserDb::open_at(&path).unwrap();
            db.record_visit("https://old.example/", "Old", 1);
            db.record_visit("https://new.example/", "New", crate::browser::tabs::now_ms());
        }
        let db = BrowserDb::open_at(&path).unwrap();
        assert_eq!(db.recent(10).len(), 1, "a 90-day-old page is gone at open");
        db.clear_since(0);
        assert_eq!(db.len(), 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_recorder_records_person_tabs_once_and_asks_for_words_after_the_dwell() {
        let db = BrowserDb::memory();
        let mut rec = Recorder::default();
        let id = super::super::TabId(7);
        assert_eq!(rec.observe(&db, id, true, "https://a.example/", "", true, T0), Note::Nothing);
        assert_eq!(rec.observe(&db, id, true, "https://a.example/", "A", true, T0 + 1_000), Note::Nothing);
        assert_eq!(db.recent(5)[0].title, "A", "the late title lands");
        assert_eq!(db.recent(5)[0].count, 1, "polling is not a visit");
        assert_eq!(rec.observe(&db, id, true, "https://a.example/", "A", true, T0 + 9_999), Note::Nothing);
        assert_eq!(rec.observe(&db, id, true, "https://a.example/", "A", true, T0 + 10_000), Note::WantText);
        assert_eq!(rec.observe(&db, id, true, "https://a.example/", "A", true, T0 + 20_000), Note::Nothing, "asked once");
        // Hidden resets the dwell.
        rec.observe(&db, id, true, "https://b.example/", "B", false, T0 + 30_000);
        assert_eq!(rec.observe(&db, id, true, "https://b.example/", "B", true, T0 + 31_000), Note::Nothing);
        assert_eq!(rec.observe(&db, id, true, "https://b.example/", "B", true, T0 + 40_000), Note::Nothing);
        assert_eq!(rec.observe(&db, id, true, "https://b.example/", "B", true, T0 + 41_000), Note::WantText);
        // A session's tab never reaches history.
        rec.observe(&db, super::super::TabId(8), false, "https://agent.example/", "Agent", true, T0);
        assert!(db.search("agent", 5).is_empty());
        assert_eq!(db.len(), 2);
    }

    #[test]
    fn ago_reads_like_the_board() {
        assert_eq!(ago(T0, T0 + 5_000), "just now");
        assert_eq!(ago(T0, T0 + 5 * 60_000), "5 min ago");
        assert_eq!(ago(T0, T0 + DAY_MS + 1), "yesterday");
        assert_eq!(ago(T0, T0 + 2 * DAY_MS + 1), "2 days ago");
        assert_eq!(ago(T0, T0 + 8 * DAY_MS), "last week");
    }

    #[test]
    fn host_of_strips_scheme_www_port_and_userinfo() {
        assert_eq!(host_of("https://www.Example.com:8443/a?b#c"), "example.com");
        assert_eq!(host_of("http://u:p@sub.example.com/"), "sub.example.com");
        assert_eq!(host_of("example.com"), "example.com");
    }
}
