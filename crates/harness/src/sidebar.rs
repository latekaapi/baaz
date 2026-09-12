//! The sessions sidebar (spec §3.7).
//!
//! `session/list` filtered to the workspace gives identity and timestamps;
//! `~/.local/share/muse/session-index.db` gives the words a person can read.
//! This module joins the two into the library's [`SessionSummary`] rows and
//! groups them by calendar date, which is the grouping the sidebar's date view
//! is built for.
//!
//! Everything here is pure: a list in, a [`Grouping`] out. The application owns
//! the fetching.

use aui_icons::Provider;
use aui_tokens::AgentState;
pub use aui::nav::Grouping;

use aui::nav::{DateGroup, SessionSummary};
use chrono::{DateTime, Datelike, Local, TimeZone, Utc};

use crate::index::IndexEntry;
use crate::sessions::SessionMeta;

/// What a session with nothing to be called is called.
///
/// The last resort, and the one Phase 4's sidebar reached fourteen times in a
/// row (finding F10). Every step before it is a real fact about the session.
pub const UNNAMED: &str = "New session";

/// One row of the sidebar, joined from the wire, the local index and the
/// harness's own overrides.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionEntry {
    /// The Muse session id.
    pub id: String,
    /// What to call it: see [`SessionEntry::join`].
    pub label: String,
    /// Last activity, as a local time.
    pub updated: DateTime<Local>,
    /// Whether this host reports the session as running.
    pub running: bool,
    /// Completed turns, shown on the meta line.
    pub turns: u64,
    /// Hidden from this window's list (`/hide`).
    pub hidden: bool,
    /// Pinned to the top of the date view, in its own group.
    pub pinned: bool,
    /// Archived out of the list (shown only from the Sessions menu).
    pub archived: bool,
    /// The muted second line: the last summary when one exists, else the
    /// first prompt — but only when the row's label does not already say it
    /// (a user-given name, or a Muse title that is not that prompt retold).
    /// Otherwise the row carries the turns meta alone, never a repeated
    /// first line.
    pub description: String,
    /// A `--replay` capture, labelled by file rather than by the index. No
    /// store source speaks for its label, so a rejoin keeps it.
    pub replayed: bool,
    /// Named with `/name` or the row's pencil. A named session with no turns
    /// is somebody's draft, not noise, so the empty filter leaves it alone.
    pub named: bool,
    /// Whether anything but the fallback was found, which is what tells the
    /// application a `session/read` is worth making (finding F10).
    pub needs_title: bool,
}

impl SessionEntry {
    /// Join one `session/list` row with what the index and the store know.
    ///
    /// The title, best first (finding F10):
    ///
    /// 1. the name someone gave it with `/name` or the row's pencil;
    /// 2. the index's `session_name`;
    /// 3. the index's generated `title`;
    /// 4. the index's `first_user_prompt`;
    /// 5. the first `userShell` command, cached in the store by the
    ///    application after a `session/read`;
    /// 6. [`UNNAMED`].
    ///
    /// The session id is never a title. "Session 01a081ef" tells a person
    /// nothing they can act on, and it reads like something went wrong.
    pub fn join(
        session: &muse_client::schema::Session,
        index: Option<&IndexEntry>,
        meta: Option<&SessionMeta>,
    ) -> Self {
        let name = meta.and_then(|m| m.name.as_deref()).map(str::trim).filter(|s| !s.is_empty());
        let indexed = index.and_then(IndexEntry::label);
        let derived = meta.and_then(|m| m.derived_title.as_deref()).map(str::trim).filter(|s| !s.is_empty());
        let label = name.or(indexed).or(derived);
        // A user-given name always earns the first prompt below it; any other
        // label earns it only when it does not already say it (see
        // `describe`): Muse writes whole first prompts into the index title,
        // and the harness's derived title is cut from the prompt the same
        // way, so comparing the fallthrough alone misses both.
        let user_named = name.is_some()
            || index
                .and_then(|i| i.session_name.as_deref())
                .map(str::trim)
                .is_some_and(|s| !s.is_empty());
        let text = label.unwrap_or(UNNAMED);
        Self {
            id: session.session_id.clone(),
            label: one_line(text),
            updated: parse_time(&session.updated_at),
            running: matches!(session.status, muse_client::schema::SessionStatus::Running),
            turns: session.turn_count,
            hidden: meta.is_some_and(|m| m.hidden),
            pinned: meta.is_some_and(|m| m.pinned),
            archived: meta.is_some_and(|m| m.archived),
            description: describe(meta, index, text, user_named),
            replayed: false,
            named: name.is_some(),
            needs_title: label.is_none(),
        }
    }

    /// The one row a `--replay` window shows: the capture it is reading.
    ///
    /// It is labelled by the file rather than by the index, because a replayed
    /// session is not one this host ever ran and the index has nothing to say
    /// about it. The timestamp is the deterministic clock, so two runs label
    /// the row the same way (see [`grouping_now`]).
    pub fn replayed(session_id: &str, capture: &std::path::Path) -> Self {
        let label = capture.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "capture".to_owned());
        Self {
            id: session_id.to_owned(),
            label,
            updated: crate::clock::now_local(),
            running: false,
            turns: 0,
            hidden: false,
            pinned: false,
            archived: false,
            description: String::new(),
            replayed: true,
            named: false,
            needs_title: false,
        }
    }

    /// Whether this row is noise: no turns yet, not running, and nobody named
    /// it. The open session is never noise — a session just created has no
    /// turns yet and must stay visible — so the caller passes its id.
    pub fn is_empty(&self, active: Option<&str>) -> bool {
        self.turns == 0 && !self.running && !self.named && !active.is_some_and(|id| id == self.id)
    }

    /// The row's state dot: running sessions pulse, everything else is idle.
    fn state(&self) -> AgentState {
        if self.running {
            AgentState::Running
        } else {
            AgentState::Idle
        }
    }

    /// The library row for this session, labelled against `now`.
    fn summary(&self, now: DateTime<Local>) -> SessionSummary {
        let mut row = SessionSummary::new(self.id.clone(), self.label.clone(), self.state(), elapsed_at(self.updated, now))
            .provider(Provider::Muse);
        // The description first, so the second line reads what was done here
        // last; the turn count stays in the meta after it.
        if !self.description.is_empty() {
            row = row.meta(aui::nav::MetaItem::Text(self.description.clone().into()));
        }
        if self.turns > 0 {
            row = row.meta(aui::nav::MetaItem::Text(
                format!("{} turn{}", self.turns, if self.turns == 1 { "" } else { "s" }).into(),
            ));
        }
        if self.archived {
            row = row.meta(aui::nav::MetaItem::Tag("Archived".into()));
        }
        if self.pinned {
            row = row.pinned();
        }
        if self.running {
            row = row.pulse();
        }
        row
    }
}

/// The single "now" a grouping is built against.
///
/// Under `HARNESS_DETERMINISTIC=1` it is the newest `updated` in the data, so
/// the newest row reads "now" however old the fixture is and two runs group
/// and label identically.
///
/// One clock per grouping, never one per row (findings `performance-6`,
/// `support-3`), and the window keys its grouping cache on the minute of it
/// (finding `support-2`), which is the finest thing an elapsed tag says.
pub fn grouping_now(entries: &[SessionEntry]) -> DateTime<Local> {
    if crate::clock::deterministic() {
        entries.iter().map(|e| e.updated).max().unwrap_or_else(crate::clock::now_local)
    } else {
        Local::now()
    }
}

/// Group the entries by calendar day, newest first, against an explicit
/// clock, so tests can pin it and the window's cache can hand back the clock
/// it keyed on. [`grouping_now`] is the clock a frame uses.
pub fn grouping_at(entries: &[SessionEntry], now: DateTime<Local>) -> Grouping {
    let mut sorted: Vec<&SessionEntry> = entries.iter().collect();
    sorted.sort_by_key(|e| std::cmp::Reverse(e.updated));
    let mut groups: Vec<DateGroup> = Vec::new();
    for entry in sorted {
        let label = bucket(entry.updated, now);
        match groups.last_mut() {
            Some(group) if group.label == label => group.sessions.push(entry.summary(now)),
            _ => groups.push(DateGroup::new(label, vec![entry.summary(now)])),
        }
    }
    Grouping::Date(groups)
}

/// Which date header a moment belongs under, on the calendar and not on a
/// rolling 24 hours: something from 00:30 this morning is "Today" at 23:00.
fn bucket(at: DateTime<Local>, now: DateTime<Local>) -> &'static str {
    let days = now.date_naive().num_days_from_ce() - at.date_naive().num_days_from_ce();
    match days {
        d if d <= 0 => "Today",
        1 => "Yesterday",
        2..=6 => "This week",
        7..=30 => "This month",
        _ => "Earlier",
    }
}

/// `now`, `14m`, `2h`, `3d` — the elapsed tag at the end of a session row,
/// against an explicit clock so one frame reads it once.
fn elapsed_at(at: DateTime<Local>, now: DateTime<Local>) -> String {
    let seconds = (now - at).num_seconds().max(0);
    match seconds {
        s if s < 60 => "now".into(),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s => format!("{}d", s / 86_400),
    }
}

/// What the row's muted second line says: the summary the last completed
/// turn left behind; without one, the index's first prompt — but only when
/// the row's label is a user-given name (`user_named`) or a Muse-provided
/// title that is not a prefix or an elision of that prompt (see
/// `echoes_prompt`). Otherwise there is no second line at all: the turns
/// meta speaks for the row. The row's own cap bounds whatever is shown.
pub fn describe(
    meta: Option<&SessionMeta>,
    index: Option<&IndexEntry>,
    label: &str,
    user_named: bool,
) -> String {
    let summary = meta.and_then(|m| m.last_summary.as_deref()).map(str::trim).filter(|s| !s.is_empty());
    if let Some(summary) = summary {
        return one_line(summary);
    }
    let prompt = index.and_then(|i| i.first_user_prompt.as_deref()).map(str::trim).filter(|s| !s.is_empty());
    let Some(prompt) = prompt else { return String::new() };
    if user_named || !echoes_prompt(label, prompt) {
        return one_line(prompt);
    }
    String::new()
}

/// Whether a row label already says the first prompt: the label, normalised
/// (lowercased, whitespace collapsed, a trailing elision trimmed), matches
/// the prompt's normalised first 40 chars in full. Catches the prompt itself,
/// a Muse index title that is the whole prompt, and a derived title elided
/// from it — all three read as the same words twice when the prompt follows.
fn echoes_prompt(label: &str, prompt: &str) -> bool {
    fn norm(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
    }
    let label: String = norm(label)
        .trim_end_matches(['\u{2026}', '.', ' '])
        .chars()
        .take(40)
        .collect();
    if label.is_empty() {
        return false;
    }
    let prompt: String = norm(prompt).chars().take(40).collect();
    prompt.starts_with(&label)
}

/// An RFC3339 instant as a local time; anything unparseable is the epoch, which
/// sorts to the bottom rather than pretending to be now.
fn parse_time(rfc3339: &str) -> DateTime<Local> {
    DateTime::parse_from_rfc3339(rfc3339)
        .map(|t| t.with_timezone(&Local))
        .unwrap_or_else(|_| Local.from_utc_datetime(&DateTime::<Utc>::UNIX_EPOCH.naive_utc()))
}

/// Sidebar rows are one line: a prompt's newlines become spaces and a very long
/// one is cut where the row would truncate it anyway.
fn one_line(text: &str) -> String {
    let flattened: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flattened.chars().count() <= 80 {
        return flattened;
    }
    flattened.chars().take(79).collect::<String>() + "\u{2026}"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(days_ago: i64) -> DateTime<Local> {
        Local::now() - chrono::Duration::days(days_ago)
    }

    #[test]
    fn buckets_are_calendar_days_not_rolling_hours() {
        let now = Local::now();
        assert_eq!(bucket(now, now), "Today");
        assert_eq!(bucket(at(1), now), "Yesterday");
        assert_eq!(bucket(at(3), now), "This week");
        assert_eq!(bucket(at(400), now), "Earlier");
    }

    #[test]
    fn a_prompt_becomes_a_single_line_row() {
        assert_eq!(one_line("why does\n  this  panic"), "why does this panic");
        assert_eq!(one_line(&"x".repeat(200)).chars().count(), 80);
    }

    #[test]
    fn an_unparseable_timestamp_sorts_last_rather_than_first() {
        assert!(parse_time("not a date") < Local::now() - chrono::Duration::days(365));
    }

    #[test]
    fn elapsed_labels_are_quantised_against_the_given_clock() {
        let now = Local::now();
        assert_eq!(elapsed_at(now, now), "now");
        assert_eq!(elapsed_at(now - chrono::Duration::seconds(90), now), "1m");
        assert_eq!(elapsed_at(now - chrono::Duration::hours(2), now), "2h");
        assert_eq!(elapsed_at(now - chrono::Duration::days(3), now), "3d");
    }

    #[test]
    fn grouping_against_a_fixed_clock_is_stable_run_to_run() {
        let now = Local::now();
        let mut fresh = entry("a");
        fresh.updated = now;
        let mut old = entry("b");
        old.updated = now - chrono::Duration::days(2);
        let entries = vec![old, fresh];
        // Twice against the same clock: identical grouping, newest first,
        // newest reading "now".
        let first = grouping_at(&entries, now);
        let second = grouping_at(&entries, now);
        assert_eq!(format!("{first:?}"), format!("{second:?}"));
    }

    /// What a sidebar frame used to cost at five hundred sessions, and what
    /// it costs now (findings `performance-5`, `support-2`).
    ///
    /// `render_sidebar` itself needs a window, so this times the two pure
    /// halves it is made of — the visible filter-and-sort and the grouping
    /// that builds one `SessionSummary` per row — against the cached frame,
    /// which is two `Rc` hand-backs and nothing else now that `sidebar_view`
    /// takes the grouping behind an `Rc` too (finding `performance-13`).
    /// Numbers with `--nocapture`; the assertion is only the ordering, so the
    /// test is not a timing flake.
    #[test]
    fn five_hundred_sidebar_rows_cost_less_from_the_cache() {
        const N: usize = 500;
        const FRAMES: usize = 20;
        let now = Local::now();
        let entries: Vec<SessionEntry> = (0..N)
            .map(|i| {
                let mut e = entry(&format!("s{i}"));
                e.label = format!("session number {i}");
                e.description = format!("did something to file {i}");
                e.turns = (i % 7) as u64;
                e.updated = now - chrono::Duration::minutes(i as i64 * 7);
                e
            })
            .collect();
        // Cold: what every frame did before — clone-and-sort the visible
        // list, then group it, building every row.
        let cold = std::time::Instant::now();
        for _ in 0..FRAMES {
            let mut visible: Vec<SessionEntry> = entries.iter().filter(|e| !e.hidden).cloned().collect();
            visible.sort_by_key(|e| std::cmp::Reverse(e.updated));
            std::hint::black_box(grouping_at(&visible, now));
        }
        let cold = cold.elapsed() / FRAMES as u32;
        // Warm: what a frame does now — the cached rows and the cached
        // grouping, both handed out behind `Rc` and neither of them cloned.
        let mut visible: Vec<SessionEntry> = entries.iter().filter(|e| !e.hidden).cloned().collect();
        visible.sort_by_key(|e| std::cmp::Reverse(e.updated));
        let visible = std::rc::Rc::new(visible);
        let grouping = std::rc::Rc::new(grouping_at(&visible, now));
        let warm = std::time::Instant::now();
        for _ in 0..FRAMES {
            std::hint::black_box(std::rc::Rc::clone(&visible));
            std::hint::black_box(std::rc::Rc::clone(&grouping));
        }
        let warm = warm.elapsed() / FRAMES as u32;
        eprintln!("sidebar-rows n={N} cold={cold:?}/frame warm={warm:?}/frame");
        assert!(warm < cold, "cached frame ({warm:?}) must cost less than the rebuild ({cold:?})");
    }

    fn entry(id: &str) -> SessionEntry {
        SessionEntry {
            id: id.to_owned(),
            label: "x".into(),
            updated: Local::now(),
            running: false,
            turns: 0,
            hidden: false,
            pinned: false,
            archived: false,
            description: String::new(),
            replayed: false,
            named: false,
            needs_title: false,
        }
    }

    #[test]
    fn a_session_with_no_turns_is_empty() {
        assert!(entry("a").is_empty(None));
    }

    #[test]
    fn the_open_session_is_never_empty() {
        assert!(!entry("a").is_empty(Some("a")));
        assert!(entry("a").is_empty(Some("b")));
    }

    #[test]
    fn a_running_session_is_never_empty() {
        let mut running = entry("a");
        running.running = true;
        assert!(!running.is_empty(None));
    }

    #[test]
    fn a_named_session_is_never_empty() {
        let mut named = entry("a");
        named.named = true;
        assert!(!named.is_empty(None));
    }

    #[test]
    fn a_session_with_turns_is_never_empty() {
        let mut turned = entry("a");
        turned.turns = 1;
        assert!(!turned.is_empty(None));
    }

    fn meta_with(summary: Option<&str>, derived: Option<&str>) -> SessionMeta {
        SessionMeta {
            last_summary: summary.map(str::to_owned),
            derived_title: derived.map(str::to_owned),
            ..SessionMeta::default()
        }
    }

    fn indexed(prompt: Option<&str>, title: &str, name: Option<&str>) -> IndexEntry {
        IndexEntry {
            session_name: name.map(str::to_owned),
            title: title.to_owned(),
            first_user_prompt: prompt.map(str::to_owned),
            ..IndexEntry::default()
        }
    }

    #[test]
    fn the_description_prefers_the_last_summary_then_the_prompt() {
        let prompt = "Run the shell command `ls` in the workspace, then use your question tool";
        // A summary wins even when the label already is the prompt.
        let meta = meta_with(Some("Fixed the parser panic"), None);
        let index = indexed(Some(prompt), prompt, None);
        assert_eq!(describe(Some(&meta), Some(&index), prompt, false), "Fixed the parser panic");
        // A Muse title that is the whole first prompt must not repeat below itself.
        assert_eq!(describe(None, Some(&index), prompt, false), "");
        // A derived title elided from the prompt is the same words twice.
        assert_eq!(describe(None, Some(&index), "Run the shell command…", false), "");
        // A user-given name earns the prompt below it.
        let named = indexed(Some(prompt), "", Some("ls run"));
        assert_eq!(describe(None, Some(&named), "ls run", true), prompt);
        // A Muse title of its own earns the prompt too.
        let titled = indexed(Some("why does this panic"), "Parser panic", None);
        assert_eq!(describe(None, Some(&titled), "Parser panic", false), "why does this panic");
        // Without a summary or a showable prompt the row carries the turns
        // meta alone; a stale derived title is not a description line.
        assert_eq!(describe(Some(&meta_with(None, Some("cargo test"))), None, "cargo test", false), "");
        assert_eq!(describe(None, None, "New session", false), "");
    }

    #[test]
    fn a_label_cut_from_the_prompt_echoes_it() {
        let prompt = "Run the shell command `ls` in the workspace, then ask";
        assert!(echoes_prompt(prompt, prompt));
        assert!(echoes_prompt("Run the shell command…", prompt));
        assert!(echoes_prompt("RUN THE SHELL   COMMAND", prompt));
        assert!(!echoes_prompt("Parser panic", "why does this panic"));
        assert!(!echoes_prompt("", prompt));
    }

    #[test]
    fn a_long_summary_is_cut_where_the_row_would_truncate_it() {
        let meta = meta_with(Some(&"w".repeat(200)), None);
        assert_eq!(describe(Some(&meta), None, "x", false).chars().count(), 80);
    }
}
