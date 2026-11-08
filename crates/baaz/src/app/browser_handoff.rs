//! Handoff from a browser chat to a project session (R3 board 3a/3b,
//! docs/27 §6): "continue this in *Fix login*", or "send this to a new
//! session in harness".
//!
//! The chat stays usable; its tabs move to the target's task folder; the
//! target gets the chat's context as an **unsent draft** in its composer
//! (the chat's last answer and the pages it used), so nothing runs and no
//! turn is spent until the person sends it.

use gpui::{div, prelude::*, Context, Window};

use super::Harness;
use crate::browser::Owner;

/// One picker row and where it hands off to.
type PickerRow = (HandoffTarget, aui::browse::handoff::HandoffRow);

/// How many recent sessions the picker offers.
const RECENT_SESSIONS: usize = 4;
/// How many projects it offers for a new session.
const PROJECTS: usize = 4;

/// Where a handoff goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HandoffTarget {
    /// An existing session, by id.
    Session(String),
    /// A new session in this project, by id.
    Project(String),
}

impl HandoffTarget {
    fn row_id(&self) -> String {
        match self {
            HandoffTarget::Session(id) => format!("s:{id}"),
            HandoffTarget::Project(id) => format!("p:{id}"),
        }
    }

    fn parse(row: &str) -> Option<Self> {
        match row.split_once(':')? {
            ("s", id) => Some(HandoffTarget::Session(id.to_owned())),
            ("p", id) => Some(HandoffTarget::Project(id.to_owned())),
            _ => None,
        }
    }
}

/// `2 h`, `yesterday`, `Tue`, `3 Nov`: how long ago, for the picker's meta.
pub(crate) fn when_label(then: chrono::DateTime<chrono::Local>, now: chrono::DateTime<chrono::Local>) -> String {
    let age = now.signed_duration_since(then);
    if age.num_minutes() < 60 {
        format!("{} min", age.num_minutes().max(1))
    } else if age.num_hours() < 24 {
        format!("{} h", age.num_hours())
    } else if age.num_days() < 2 {
        "yesterday".into()
    } else if age.num_days() < 7 {
        then.format("%a").to_string()
    } else {
        then.format("%-d %b").to_string()
    }
}

/// The draft the target's composer gets: the chat's title, its last answer
/// and the pages it used. Plain words, written to be edited before sending.
pub(crate) fn handoff_draft(title: &str, answer: Option<&str>, pages: &[(String, String)]) -> String {
    let mut out = format!("From my browser chat \u{201C}{title}\u{201D}:\n\n");
    if let Some(answer) = answer.filter(|a| !a.trim().is_empty()) {
        out.push_str(answer.trim());
        out.push_str("\n\n");
    }
    if !pages.is_empty() {
        out.push_str("Pages it used:\n");
        for (title, url) in pages {
            if title.is_empty() || title == url {
                out.push_str(&format!("- {url}\n"));
            } else {
                out.push_str(&format!("- {title}: {url}\n"));
            }
        }
        out.push('\n');
    }
    out
}

impl Harness {
    /// The picker's rows: recent sessions (not browser chats, not hidden),
    /// then projects, filtered by `query`.
    fn handoff_rows(&self, query: &str) -> (Vec<PickerRow>, Vec<PickerRow>) {
        use aui::browse::handoff::HandoffRow;
        let query = query.trim().to_lowercase();
        let hit = |text: &str| query.is_empty() || text.to_lowercase().contains(&query);
        let now = chrono::Local::now();
        let mut entries: Vec<&crate::sidebar::SessionEntry> = self
            .sessions
            .iter()
            .filter(|e| !e.hidden && !e.archived && !e.side_marker && !self.is_browser_chat(&e.id))
            .collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.updated));
        let sessions = entries
            .into_iter()
            .filter_map(|e| {
                let project = self.project_name_for(&e.id).unwrap_or_default();
                (hit(&e.label) || hit(&project)).then(|| {
                    let meta = if project.is_empty() { when_label(e.updated, now) } else { format!("{project} · {}", when_label(e.updated, now)) };
                    let target = HandoffTarget::Session(e.id.clone());
                    let row = HandoffRow { id: target.row_id().into(), title: e.label.clone().into(), meta: meta.into(), provider: Some(e.provider.icon()) };
                    (target, row)
                })
            })
            .take(RECENT_SESSIONS)
            .collect();
        let provider = crate::providers::ProviderId::parse(&self.new_provider);
        let projects = self
            .projects
            .sorted_available()
            .into_iter()
            .filter(|p| hit(&p.name))
            .take(PROJECTS)
            .map(|p| {
                let target = HandoffTarget::Project(p.id.clone());
                let row = HandoffRow { id: target.row_id().into(), title: p.name.clone().into(), meta: provider.label().into(), provider: None };
                (target, row)
            })
            .collect();
        (sessions, projects)
    }

    /// The picker over the card, while it is open: anchored under the
    /// card's ↗, right-aligned to it.
    pub(crate) fn render_handoff_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.handoff_picker_open {
            return None;
        }
        let query = self
            .handoff_query
            .get_or_insert_with(|| cx.new(|cx| aui::composer::composer_state_rows("Find a session or project", 1, 1, window, cx)))
            .clone();
        let text = query.read(cx).value().to_string();
        let (sessions, projects) = self.handoff_rows(&text);
        let total = sessions.len() + projects.len();
        let selected = self.handoff_selected.min(total.saturating_sub(1));
        let rows_s: Vec<_> = sessions.iter().map(|(_, row)| row.clone()).collect();
        let rows_p: Vec<_> = projects.iter().map(|(_, row)| row.clone()).collect();
        let baaz = cx.weak_entity();
        let picker = aui::browse::handoff::handoff_picker("handoff-picker", &query, rows_s, rows_p, selected).on_intent(move |intent, _, cx| {
            use aui::browse::handoff::HandoffIntent;
            let _ = baaz.update(cx, |this, cx| match intent {
                HandoffIntent::Choose(row) => {
                    if let Some(target) = HandoffTarget::parse(&row) {
                        this.run_on_main(cx, move |this, window, cx| this.hand_off_browser_chat(target, window, cx));
                    }
                }
                HandoffIntent::Hover(index) => {
                    this.handoff_selected = index;
                    cx.notify();
                }
                HandoffIntent::Close => {
                    this.handoff_picker_open = false;
                    cx.notify();
                }
            });
        });
        let keys = cx.listener(move |this: &mut Self, event: &gpui::KeyDownEvent, _, cx| match event.keystroke.key.as_str() {
            "up" => {
                this.handoff_selected = this.handoff_selected.saturating_sub(1);
                cx.notify();
            }
            "down" => {
                this.handoff_selected = (this.handoff_selected + 1).min(total.saturating_sub(1));
                cx.notify();
            }
            "enter" => {
                let text = this.handoff_query.as_ref().map(|q| q.read(cx).value().to_string()).unwrap_or_default();
                let (sessions, projects) = this.handoff_rows(&text);
                let target = sessions.into_iter().chain(projects).nth(this.handoff_selected).map(|(t, _)| t);
                if let Some(target) = target {
                    this.run_on_main(cx, move |this, window, cx| this.hand_off_browser_chat(target, window, cx));
                }
            }
            "escape" => {
                this.handoff_picker_open = false;
                cx.notify();
            }
            _ => {}
        });
        // Under the card's header, its right edge 70 in from the card's
        // (the ↗ sits left of Dock and Close).
        Some(div().absolute().top(gpui::px(crate::overlay::SHADOW_PAD + 44.0)).right(gpui::px(crate::overlay::SHADOW_PAD + 62.0)).on_key_down(keys).child(picker).into_any_element())
    }

    /// Hand the browser chat off to `target`: its tabs move to the target's
    /// task folder, the target opens in Sessions with the chat's context as
    /// an unsent draft, and the chat records the handoff (board 3b).
    pub(crate) fn hand_off_browser_chat(&mut self, target: HandoffTarget, window: &mut Window, cx: &mut Context<Self>) {
        self.handoff_picker_open = false;
        let Some(chat) = self.browser_chat.clone() else { return };
        let (chat_id, answer) = {
            let view = chat.read(cx);
            let busy = view.busy() || view.is_starting();
            (view.session_id.clone(), view.transcript_session().and_then(|s| crate::app::agent_window::settled_answer(s, busy)))
        };
        let title = self.browser_chat_title_for(&chat_id, cx);
        let owner = Owner::Session(chat_id.clone());
        let tabs: Vec<_> = self.browser.tabs.tabs_of(&owner).iter().map(|tab| (tab.id, tab.title.clone(), tab.url.clone())).collect();
        // The person's pages it was asked about, then the tabs it opened.
        let mut pages: Vec<(String, String)> = self.browser_chat_pages.get(&chat_id).cloned().unwrap_or_default();
        for (_, title, url) in &tabs {
            if !pages.iter().any(|(_, u)| u == url) {
                pages.push((title.clone(), url.clone()));
            }
        }
        let draft = handoff_draft(&title, answer.as_deref(), &pages);
        // The target: an existing session, or a new one in the project.
        let (target_id, target_label, project_label) = match &target {
            HandoffTarget::Session(id) => {
                let label = self.sessions.iter().find(|e| &e.id == id).map(|e| e.label.clone()).unwrap_or_default();
                let project = self.project_name_for(id).unwrap_or_default();
                self.pending_ask_draft = Some(crate::app::PendingAsk { target: Some(id.clone()), query: draft });
                self.set_mode(crate::layout::Mode::Sessions, window, cx);
                if self.active_id(cx).as_deref() != Some(id.as_str()) {
                    self.resume(id.clone(), window, cx);
                } else if let Some(view) = self.active.clone() {
                    // Already on screen: the draft goes straight into its composer.
                    let text = self.pending_ask_draft.take().map(|p| p.query).unwrap_or_default();
                    let existing = view.read(cx).draft_text(cx);
                    let next = crate::browser::toolbar::ask_draft_text(&existing, &text);
                    view.update(cx, |view, cx| view.set_draft(next, window, cx));
                }
                (Some(id.clone()), label, project)
            }
            HandoffTarget::Project(project) => {
                let name = self.projects.find(project).map(|p| p.name.clone()).unwrap_or_default();
                self.pending_ask_draft = Some(crate::app::PendingAsk { target: None, query: draft });
                self.new_session_in(Some(project.clone()), window, cx);
                (self.active_id(cx), "a new session".to_owned(), name)
            }
        };
        if let Some(target_id) = target_id.as_ref() {
            for (tab, _, _) in &tabs {
                self.browser.tabs.move_to(*tab, Owner::Session(target_id.clone()));
            }
            self.schedule_tabs_write(cx);
        }
        self.terminal_service.revoke_grants(&chat_id);
        let n = tabs.len();
        let detail = match (project_label.is_empty(), n) {
            (_, 0) => project_label.clone(),
            (true, n) => format!("its {n} {} now in that session's task folder", if n == 1 { "tab is" } else { "tabs are" }),
            (false, n) => format!("{project_label} · its {n} {} now in that session's task folder", if n == 1 { "tab is" } else { "tabs are" }),
        };
        self.browser_handoffs.insert(chat_id, (target_id.unwrap_or_default(), target_label, detail));
        cx.notify();
    }

    /// Open the session the chat was handed off to (the card's Open).
    pub(crate) fn open_handoff_target(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = self.browser_chat_id(cx) else { return };
        let Some((target, _, _)) = self.browser_handoffs.get(&chat).cloned() else { return };
        if target.is_empty() {
            return;
        }
        self.set_mode(crate::layout::Mode::Sessions, window, cx);
        if self.active_id(cx).as_deref() != Some(target.as_str()) {
            self.resume(target, window, cx);
        }
    }

    /// The card's title for chat `session` (see the overlay's own).
    pub(crate) fn browser_chat_title_for(&self, session: &str, cx: &gpui::App) -> String {
        if let Some(meta) = self.overrides.get(session) {
            if let Some(title) = meta.name.clone().or_else(|| meta.generated_title.clone()) {
                return title;
            }
        }
        self.find_view(session, cx)
            .and_then(|view| {
                view.read(cx).transcript_session().and_then(|s| {
                    s.turns.iter().find_map(|turn| match turn {
                        aui_protocol::Turn::User { text, .. } => {
                            Some(crate::app::agent_window::strip_page_prompt(text).lines().next().unwrap_or("").to_owned())
                        }
                        _ => None,
                    })
                })
            })
            .unwrap_or_else(|| "Browser chat".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_draft_carries_the_answer_and_the_pages() {
        let draft = handoff_draft("Refund policies", Some("30 days at shop."), &[("Returns".into(), "https://shop.example/r".into())]);
        assert!(draft.starts_with("From my browser chat \u{201C}Refund policies\u{201D}"));
        assert!(draft.contains("30 days at shop."));
        assert!(draft.contains("- Returns: https://shop.example/r"));
        let bare = handoff_draft("x", None, &[]);
        assert_eq!(bare, "From my browser chat \u{201C}x\u{201D}:\n\n");
    }

    #[test]
    fn targets_round_trip_through_row_ids() {
        for target in [HandoffTarget::Session("abc".into()), HandoffTarget::Project("p1".into())] {
            assert_eq!(HandoffTarget::parse(&target.row_id()), Some(target));
        }
        assert_eq!(HandoffTarget::parse("q:x"), None);
    }

    #[test]
    fn when_reads_like_the_board() {
        use chrono::{Duration, TimeZone};
        let now = chrono::Local.with_ymd_and_hms(2026, 11, 8, 15, 0, 0).unwrap();
        assert_eq!(when_label(now - Duration::hours(2), now), "2 h");
        assert_eq!(when_label(now - Duration::hours(30), now), "yesterday");
        assert_eq!(when_label(now - Duration::minutes(5), now), "5 min");
    }
}
