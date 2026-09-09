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
use aui::nav::{DateGroup, Grouping, SessionSummary};
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
    /// Named with `/name` or the row's pencil. A named session with no turns
    /// is somebody's draft, not noise, so the empty filter leaves it alone.
    pub named: bool,
    /// Everything the search field matches against: the label, the index's
    /// title and first prompt, and whatever Muse made searchable.
    pub haystack: String,
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
        Self {
            id: session.session_id.clone(),
            label: one_line(label.unwrap_or(UNNAMED)),
            updated: parse_time(&session.updated_at),
            running: matches!(session.status, muse_client::schema::SessionStatus::Running),
            turns: session.turn_count,
            hidden: meta.is_some_and(|m| m.hidden),
            named: name.is_some(),
            haystack: haystack(label, index),
            needs_title: label.is_none(),
        }
    }

    /// Whether the search field's text matches this row.
    pub fn matches(&self, needle: &str) -> bool {
        crate::sessions::matches(&self.haystack, needle)
    }

    /// The one row a `--replay` window shows: the capture it is reading.
    ///
    /// It is labelled by the file rather than by the index, because a replayed
    /// session is not one this host ever ran and the index has nothing to say
    /// about it.
    pub fn replayed(session_id: &str, capture: &std::path::Path) -> Self {
        let label = capture.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "capture".to_owned());
        Self {
            id: session_id.to_owned(),
            haystack: label.clone(),
            label,
            updated: Local::now(),
            running: false,
            turns: 0,
            hidden: false,
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

    /// The library row for this session.
    fn summary(&self) -> SessionSummary {
        let mut row = SessionSummary::new(self.id.clone(), self.label.clone(), self.state(), elapsed(self.updated))
            .provider(Provider::Muse);
        if self.turns > 0 {
            row = row.meta(aui::nav::MetaItem::Text(
                format!("{} turn{}", self.turns, if self.turns == 1 { "" } else { "s" }).into(),
            ));
        }
        if self.running {
            row = row.pulse();
        }
        row
    }
}

/// Group the entries by calendar day, newest first, into the date view.
pub fn grouping(entries: &[SessionEntry]) -> Grouping {
    let mut sorted: Vec<&SessionEntry> = entries.iter().collect();
    sorted.sort_by_key(|e| std::cmp::Reverse(e.updated));
    let mut groups: Vec<DateGroup> = Vec::new();
    for entry in sorted {
        let label = bucket(entry.updated, Local::now());
        match groups.last_mut() {
            Some(group) if group.label == label => group.sessions.push(entry.summary()),
            _ => groups.push(DateGroup::new(label, vec![entry.summary()])),
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

/// `now`, `14m`, `2h`, `3d` — the elapsed tag at the end of a session row.
fn elapsed(at: DateTime<Local>) -> String {
    let seconds = (Local::now() - at).num_seconds().max(0);
    match seconds {
        s if s < 60 => "now".into(),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s => format!("{}d", s / 86_400),
    }
}

/// An RFC3339 instant as a local time; anything unparseable is the epoch, which
/// sorts to the bottom rather than pretending to be now.
fn parse_time(rfc3339: &str) -> DateTime<Local> {
    DateTime::parse_from_rfc3339(rfc3339)
        .map(|t| t.with_timezone(&Local))
        .unwrap_or_else(|_| Local.from_utc_datetime(&DateTime::<Utc>::UNIX_EPOCH.naive_utc()))
}

/// Everything the search field looks in: the label plus every other word the
/// index made searchable about the session.
fn haystack(label: Option<&str>, index: Option<&IndexEntry>) -> String {
    let mut text = String::new();
    let mut push = |part: &str| {
        if part.trim().is_empty() {
            return;
        }
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(part.trim());
    };
    if let Some(label) = label {
        push(label);
    }
    if let Some(index) = index {
        push(&index.title);
        if let Some(prompt) = &index.first_user_prompt {
            push(prompt);
        }
        push(&index.search_text);
    }
    text
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

    fn entry(id: &str) -> SessionEntry {
        SessionEntry {
            id: id.to_owned(),
            label: "x".into(),
            updated: Local::now(),
            running: false,
            turns: 0,
            hidden: false,
            named: false,
            haystack: "x".into(),
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
}
