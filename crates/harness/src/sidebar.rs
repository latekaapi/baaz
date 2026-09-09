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

/// One row of the sidebar, joined from the wire and the local index.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionEntry {
    /// The Muse session id.
    pub id: String,
    /// What to call it: the index's name, title or first prompt, or the id.
    pub label: String,
    /// Last activity, as a local time.
    pub updated: DateTime<Local>,
    /// Whether this host reports the session as running.
    pub running: bool,
    /// Completed turns, shown on the meta line.
    pub turns: u64,
}

impl SessionEntry {
    /// Join one `session/list` row with what the index knows about it.
    pub fn join(session: &muse_client::schema::Session, index: Option<&IndexEntry>) -> Self {
        let label = index
            .and_then(IndexEntry::label)
            .map(str::to_owned)
            .unwrap_or_else(|| short_id(&session.session_id));
        Self {
            id: session.session_id.clone(),
            label: one_line(&label),
            updated: parse_time(&session.updated_at),
            running: matches!(session.status, muse_client::schema::SessionStatus::Running),
            turns: session.turn_count,
        }
    }

    /// The one row a `--replay` window shows: the capture it is reading.
    ///
    /// It is labelled by the file rather than by the index, because a replayed
    /// session is not one this host ever ran and the index has nothing to say
    /// about it.
    pub fn replayed(session_id: &str, capture: &std::path::Path) -> Self {
        let label = capture.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "capture".to_owned());
        Self { id: session_id.to_owned(), label, updated: Local::now(), running: false, turns: 0 }
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

/// A session id shortened to its first group, for a session the index has never
/// heard of.
fn short_id(id: &str) -> String {
    format!("Session {}", id.split('-').next().unwrap_or(id))
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
}
