//! Handoff from a browser chat to a project session (R3 board 3a/3b,
//! docs/27 §6): "continue this in *Fix login*", or "send this to a new
//! session in harness".
//!
//! The chat stays usable; its tabs move to the target's task folder; the
//! target gets the chat's context as an **unsent draft** in its composer
//! (the chat's last answer and the pages it used), so nothing runs and no
//! turn is spent until the person sends it. The person stays on the page: a
//! toast says where it went, with Open (docs/82 8.2).
//!
//! The same picker, centred over the window, asks "Add to which session?"
//! for a screenshot, a page's notes, a quote or terminal output when no
//! composer is on screen ([`crate::browser::Parcel`]).

use gpui::{div, prelude::*, Context, Window};

use super::Harness;
use crate::browser::Owner;
use crate::providers::ProviderMark as _;

/// The hand-off picker: one [`crate::picker::Picker`] over the card.
pub(crate) type HandoffPicker = crate::picker::Picker<HandoffDelegate>;

/// One row the picker may list, where it hands off to, and the words the
/// query matches (a session's label and project, a project's name).
#[derive(Clone)]
pub(crate) struct Candidate {
    target: HandoffTarget,
    row: aui::browse::handoff::HandoffRow,
    words: Vec<String>,
}

/// Everything the picker may list, pushed by the root.
#[derive(Clone, Default)]
pub(crate) struct HandoffCandidates {
    sessions: Vec<Candidate>,
    projects: Vec<Candidate>,
}

/// The hand-off picker's delegate: recent sessions then projects, each
/// narrowed by the query (case-insensitive, on label and project) and
/// capped ([`RECENT_SESSIONS`], [`PROJECTS`]).
pub(crate) struct HandoffDelegate {
    candidates: HandoffCandidates,
    /// The sessions and the projects the query leaves, in order.
    matches: (Vec<usize>, Vec<usize>),
    /// Where it stands and what it is for.
    seat: PickerSeat,
}

/// Where the picker stands, which says what it is for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickerSeat {
    /// Under the agent card's header: the card's hand-off.
    Card,
    /// Centred over the window like the palette, on a scrim: "Add to which
    /// session?" for what is bound for a composer no one can see (docs/82
    /// 8.2), its footnote naming what goes.
    Attach(&'static str),
}

/// The attach picker's footnote for `noun` ([`crate::browser::Parcel::noun`]).
fn attach_note(noun: &str) -> &'static str {
    match noun {
        "Notes" => "Add these notes to which session? They wait in its composer, unsent.",
        "Quote" => "Add this quote to which session? It waits in its composer, unsent.",
        "Terminal output" => "Add this output to which session? It waits in its composer, unsent.",
        "Page" => "Add this page to which session? It waits in its composer, unsent.",
        "Selection" => "Add the selection to which session? It waits in its composer, unsent.",
        _ => "Add this screenshot to which session? It waits in its composer, unsent.",
    }
}

impl HandoffDelegate {
    /// Match `ix`, sessions first.
    fn candidate(&self, ix: usize) -> Option<&Candidate> {
        let (sessions, projects) = &self.matches;
        match sessions.get(ix) {
            Some(at) => self.candidates.sessions.get(*at),
            None => projects.get(ix - sessions.len()).and_then(|at| self.candidates.projects.get(*at)),
        }
    }
}

impl crate::picker::PickerDelegate for HandoffDelegate {
    type Row = aui::browse::handoff::HandoffRow;
    type Pick = HandoffTarget;

    fn match_count(&self) -> usize {
        self.matches.0.len() + self.matches.1.len()
    }

    fn update_matches(&mut self, query: &str) {
        let query = query.trim().to_lowercase();
        let hit = |c: &Candidate| query.is_empty() || c.words.iter().any(|word| word.to_lowercase().contains(&query));
        let pick = |list: &[Candidate], cap: usize| list.iter().enumerate().filter(|(_, c)| hit(c)).map(|(at, _)| at).take(cap).collect();
        self.matches = (pick(&self.candidates.sessions, RECENT_SESSIONS), pick(&self.candidates.projects, PROJECTS));
    }

    fn match_id(&self, ix: usize) -> Option<gpui::SharedString> {
        self.candidate(ix).map(|c| c.row.id.clone())
    }

    fn confirm(&self, ix: usize) -> Option<HandoffTarget> {
        self.candidate(ix).map(|c| c.target.clone())
    }

    fn confirm_id(&self, id: &gpui::SharedString) -> Option<HandoffTarget> {
        HandoffTarget::parse(id)
    }

    fn render_match(&self, ix: usize, _selected: bool) -> Self::Row {
        self.candidate(ix).map(|c| c.row.clone()).expect("a match in range")
    }

    fn render_picker(
        &self,
        mut rows: Vec<Self::Row>,
        frame: crate::picker::PickerFrame,
        _window: &mut Window,
        _cx: &mut Context<HandoffPicker>,
    ) -> gpui::AnyElement {
        use aui::browse::handoff::HandoffIntent;
        let projects = rows.split_off(self.matches.0.len().min(rows.len()));
        let Some(field) = frame.field.clone() else { return div().into_any_element() };
        let (select, hover, dismiss) = (frame.select.clone(), frame.hover.clone(), frame.dismiss.clone());
        let id = if self.seat == PickerSeat::Card { "handoff-picker" } else { "attach-picker" };
        let picker = aui::browse::handoff::handoff_picker(id, &field, rows, projects, frame.selected).on_intent(
            move |intent, window, cx| match intent {
                HandoffIntent::Choose(row) => select(&row, window, cx),
                HandoffIntent::Hover(index) => hover(index, window, cx),
                HandoffIntent::Close => dismiss(window, cx),
            },
        );
        match self.seat {
            // Under the card's header, its right edge 70 in from the card's
            // (the ↗ sits left of Dock and Close).
            PickerSeat::Card => frame
                .keyed(div().absolute().top(gpui::px(crate::overlay::SHADOW_PAD + 44.0)).right(gpui::px(crate::overlay::SHADOW_PAD + 62.0)))
                .child(picker)
                .into_any_element(),
            // Centred, its top at 18 % of the window like the palette
            // (docs/83 §7), on the scrim: a click there cancels — nothing
            // is attached anywhere.
            PickerSeat::Attach(note) => {
                let dismiss = frame.dismiss.clone();
                let layer = aui::interact::dismiss_layer("attach-picker-scrim", "Cancel: add to no session", move |window, cx| dismiss(window, cx));
                aui::overlay::popover_layer(
                    frame.keyed(layer).bg(aui::overlay::scrim()).child(
                        aui::util::v_flex()
                            .size_full()
                            .items_center()
                            .child(div().flex_none().w_full().h(gpui::relative(aui_tokens::scale::PALETTE_TOP)))
                            .child(picker.note(note).headings("Recent sessions", "New session in")),
                    ),
                )
                .into_any_element()
            }
        }
    }
}

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
    /// The picker's candidates: recent sessions (not browser chats, not
    /// hidden), newest first, then projects — each with the words the
    /// query matches. The picker narrows them ([`HandoffDelegate`]).
    fn handoff_candidates(&self, cx: &gpui::App) -> HandoffCandidates {
        use aui::browse::handoff::HandoffRow;
        let now = chrono::Local::now();
        let mut entries: Vec<&crate::sidebar::SessionEntry> = self
            .session_list.read(cx).sessions
            .iter()
            .filter(|e| !e.hidden && !e.archived && !e.side_marker && !self.is_browser_chat(&e.id, cx))
            .collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.updated));
        let sessions = entries
            .into_iter()
            .map(|e| {
                let project = self.project_name_for(&e.id, cx).unwrap_or_default();
                let meta = if project.is_empty() { when_label(e.updated, now) } else { format!("{project} · {}", when_label(e.updated, now)) };
                let target = HandoffTarget::Session(e.id.clone());
                let row = HandoffRow { id: target.row_id().into(), title: e.label.clone().into(), meta: meta.into(), provider: Some(e.provider.icon()) };
                Candidate { target, row, words: vec![e.label.clone(), project] }
            })
            .collect();
        let provider = self.new_provider_id(cx).map_or("", |provider| provider.label());
        let projects = self
            .session_list.read(cx).projects
            .sorted_available()
            .into_iter()
            .map(|p| {
                let target = HandoffTarget::Project(p.id.clone());
                let row = HandoffRow { id: target.row_id().into(), title: p.name.clone().into(), meta: provider.into(), provider: None };
                Candidate { target, row, words: vec![p.name.clone()] }
            })
            .collect();
        HandoffCandidates { sessions, projects }
    }

    /// Open (or close) the picker over the card.
    pub(crate) fn toggle_handoff_picker(&mut self, cx: &mut Context<Self>) {
        if let Some(view) = self.handoff_picker_view(cx) {
            view.update(cx, |picker, cx| picker.dismiss(cx));
            return;
        }
        let delegate = HandoffDelegate { candidates: self.handoff_candidates(cx), matches: (Vec::new(), Vec::new()), seat: PickerSeat::Card };
        let view = cx.new(|cx| crate::picker::Picker::with_own_field(delegate, "Find a session or project", cx));
        let picked = cx.subscribe(&view, |this: &mut Self, _, event: &crate::picker::Picked<HandoffTarget>, cx| {
            let target = event.0.clone();
            this.run_on_main(cx, move |this, window, cx| this.hand_off_browser_chat(target, window, cx));
        });
        let entry = crate::overlay_stack::Entry::new(crate::overlay_stack::Layer::HandoffPicker).keep(picked);
        self.push_overlay_view(entry, &view, cx);
    }

    /// The open hand-off picker's view.
    pub(crate) fn handoff_picker_view(&self, cx: &gpui::App) -> Option<gpui::Entity<HandoffPicker>> {
        self.overlays.read(cx).stack.view_as::<HandoffPicker>(crate::overlay_stack::LayerKind::HandoffPicker)
    }

    /// The picker over the card, while it is open — its candidates pushed
    /// first (it draws right after, in this same frame).
    pub(crate) fn render_handoff_picker(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let view = self.handoff_picker_view(cx)?;
        let candidates = self.handoff_candidates(cx);
        view.update(cx, |picker, cx| picker.push_facts(cx, |delegate| delegate.candidates = candidates));
        Some(view.into_any_element())
    }

    /// "Add to which session?" (docs/82 8.2): the hand-off picker's rows,
    /// centred over the window, holding `parcel` in its overlay entry until
    /// a pick delivers it ([`Self::attach_picked`]). Escape or a click away
    /// drops the entry and the parcel with it: nothing is attached. A
    /// picker already open gives way to this one.
    pub(crate) fn open_attach_picker(&mut self, parcel: crate::browser::Parcel, cx: &mut Context<Self>) {
        let seat = PickerSeat::Attach(attach_note(parcel.noun()));
        let delegate = HandoffDelegate { candidates: self.handoff_candidates(cx), matches: (Vec::new(), Vec::new()), seat };
        let view = cx.new(|cx| crate::picker::Picker::with_own_field(delegate, "Add to which session?", cx));
        let picked = cx.subscribe(&view, |this: &mut Self, _, event: &crate::picker::Picked<HandoffTarget>, cx| {
            let target = event.0.clone();
            this.run_on_main(cx, move |this, window, cx| this.attach_picked(target, window, cx));
        });
        let entry = crate::overlay_stack::Entry::new(crate::overlay_stack::Layer::AttachPicker(Box::new(parcel))).keep(picked);
        self.push_overlay_view(entry, &view, cx);
    }

    /// The open attach picker's view.
    pub(crate) fn attach_picker_view(&self, cx: &gpui::App) -> Option<gpui::Entity<HandoffPicker>> {
        self.overlays.read(cx).stack.view_as::<HandoffPicker>(crate::overlay_stack::LayerKind::AttachPicker)
    }

    /// The attach picker for this frame, its candidates pushed first (it
    /// draws right after, in this same frame), or `None` when none is open.
    pub(crate) fn compose_attach_picker(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let view = self.attach_picker_view(cx)?;
        let candidates = self.handoff_candidates(cx);
        view.update(cx, |picker, cx| picker.push_facts(cx, |delegate| delegate.candidates = candidates));
        Some(view.into_any_element())
    }

    /// The row ids the attach picker would list now, for the hosted
    /// modal's print: a row landing redraws the panel.
    pub(crate) fn attach_picker_print(&self, cx: &gpui::App) -> Option<Vec<gpui::SharedString>> {
        self.attach_picker_view(cx)?;
        let candidates = self.handoff_candidates(cx);
        Some(candidates.sessions.iter().chain(&candidates.projects).map(|c| c.row.id.clone()).collect())
    }

    /// Hand the browser chat off to `target`: its tabs move to the target's
    /// task folder, the target gets the chat's context as an unsent draft
    /// ([`crate::browser::Parcel::Text`]), a toast says where it went with
    /// Open, and the chat records the handoff (board 3b). The person stays
    /// on the page; a new session opens in its project, there being no new
    /// session without opening one.
    pub(crate) fn hand_off_browser_chat(&mut self, target: HandoffTarget, window: &mut Window, cx: &mut Context<Self>) {
        self.overlays.update(cx, |o, cx| { o.close(crate::overlay_stack::LayerKind::HandoffPicker, cx); });
        let Some(chat) = self.registry.browser_chat().cloned() else { return };
        let (chat_id, answer) = {
            let view = chat.read(cx);
            let busy = view.rt(cx).busy() || view.rt(cx).is_starting();
            (view.rt(cx).session_id.clone(), view.rt(cx).transcript_session().and_then(|s| crate::app::agent_window::settled_answer(s, busy)))
        };
        let title = self.browser_chat_title_for(&chat_id, cx);
        let owner = Owner::Session(chat_id.clone());
        let tabs: Vec<_> = self.browser.read(cx).tabs.tabs_of(&owner).iter().map(|tab| (tab.id, tab.title.clone(), tab.url.clone())).collect();
        // The person's pages it was asked about, then the tabs it opened.
        let mut pages: Vec<(String, String)> = self.browser.read(cx).agent_window.browser_chat_pages.get(&chat_id).cloned().unwrap_or_default();
        for (_, title, url) in &tabs {
            if !pages.iter().any(|(_, u)| u == url) {
                pages.push((title.clone(), url.clone()));
            }
        }
        let draft = handoff_draft(&title, answer.as_deref(), &pages);
        // The target: an existing session (its chain's head, where it
        // opens), or a new one in the project.
        let (target_label, project_label) = match &target {
            HandoffTarget::Session(id) => {
                let head = self.chain_head(id, cx);
                let label = self.session_list.read(cx).sessions.iter().find(|e| e.id == head).map(|e| e.label.clone()).unwrap_or_default();
                (label, self.project_name_for(&head, cx).unwrap_or_default())
            }
            HandoffTarget::Project(project) => {
                ("a new session".to_owned(), self.session_list.read(cx).projects.find(project).map(|p| p.name.clone()).unwrap_or_default())
            }
        };
        // Its tabs follow the draft — when the session is known now; a new
        // session whose open is still in flight leaves them with the chat
        // (never on whatever `active` happens to be).
        let target_id = self.deliver_parcel(target, crate::browser::Parcel::Text(draft), window, cx);
        if let Some(target_id) = target_id.as_ref() {
            for (tab, _, _) in &tabs {
                self.browser.update(cx, |browser, _| browser.tabs.move_to(*tab, Owner::Session(target_id.clone()), None));
            }
            self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
        }
        self.terminal_service.revoke_grants(&chat_id);
        let n = tabs.len();
        let detail = match (project_label.is_empty(), n) {
            (_, 0) => project_label.clone(),
            (true, n) => format!("its {n} {} now in that session's task folder", if n == 1 { "tab is" } else { "tabs are" }),
            (false, n) => format!("{project_label} · its {n} {} now in that session's task folder", if n == 1 { "tab is" } else { "tabs are" }),
        };
        self.browser.update(cx, |browser, _| browser.agent_window.browser_handoffs.insert(chat_id, (target_id.unwrap_or_default(), target_label, detail)));
        cx.notify();
    }

    /// Open the session the chat was handed off to (the card's Open).
    pub(crate) fn open_handoff_target(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = self.browser_chat_id(cx) else { return };
        let Some((target, _, _)) = self.browser.read(cx).agent_window.browser_handoffs.get(&chat).cloned() else { return };
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
        if let Some(meta) = self.session_list.read(cx).overrides.get(session) {
            if let Some(title) = meta.name.clone().or_else(|| meta.generated_title.clone()) {
                return title;
            }
        }
        self.find_view(session, cx)
            .and_then(|view| {
                view.read(cx).rt(cx).transcript_session().and_then(|s| {
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

#[cfg(all(test, feature = "probe"))]
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
