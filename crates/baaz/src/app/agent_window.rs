//! The agent window over a Browse page (docs/27 §4.5, R3): the ⌘J line, the
//! ⌘E card and the ⌘⇧E column, wired to a browser chat
//! ([`super::browser_chat`]). The library draws every state (agentic-ui
//! `browse/line-*`, `browse/agent-card`, `browse/agent-column`,
//! `browse/handoff`); this module decides which one shows and turns their
//! intents into Baaz actions.
//!
//! - **The line** asks about the page on screen. A small ask answers inline
//!   (the line grows into an answer, R3 board 1a/1b); an ask that takes more
//!   than a few steps grows into the card. "Draft a reply" writes into the
//!   page's own box and never sends (board 2a/2b).
//! - **The card** is the chat itself, bottom-right over the page; **the
//!   column** is the same card docked beside the page.
//! - While the chat (or any session) acts on the tab on screen, the line is
//!   the driving bar (R2), whatever this module's own state.

use gpui::{AppContext as _, Context, Window};

use super::Harness;

/// What the line shows when no agent is driving the tab on screen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum LineUi {
    /// Closed (the page alone).
    #[default]
    Closed,
    /// The ask row with its chips and the site's actions.
    Ask,
    /// A small ask answering on the line: `question` as sent to `session`.
    Answer { question: String, session: String },
    /// A draft written into the page's box by `session`, on `tab`.
    Drafting { session: String, tab: u64 },
    /// The draft landed; nothing was sent.
    Drafted { session: String, tab: u64 },
}

/// Where the card stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum CardUi {
    /// Not shown.
    #[default]
    Closed,
    /// The floating card, bottom-right over the page.
    Card,
    /// Docked as a column beside the page.
    Column,
}

/// The column's width, beside the page.
pub(crate) const COLUMN_W: f32 = 400.0;
/// An ask on the line whose turn takes more steps than this grows into the
/// card (docs/27 §4.5: "any multi-step ask").
pub(crate) const INLINE_MAX_STEPS: usize = 3;

/// The site actions under the ask row (docs/27 §4.5).
pub(crate) const SITE_ACTIONS: [(&str, &str); 2] = [("sum", "Sum it up"), ("draft", "Draft a reply")];

/// The prompt that tells a browser chat which page the person means, ahead
/// of their words. The tab is theirs: the grant lets the chat use it.
pub(crate) fn page_prompt(ask: &str, page: Option<(u64, &str, &str)>) -> String {
    match page {
        Some((tab, title, url)) => format!(
            "I'm looking at tab {tab} in my browser: \u{201C}{title}\u{201D} ({url}). Use the browser tools with tab={tab} to read it; open tabs of your own with browser_tabs for anything else.\n\n{ask}"
        ),
        None => ask.to_owned(),
    }
}

/// The site action's ask, in the person's words.
pub(crate) fn site_action_ask(id: &str) -> Option<&'static str> {
    match id {
        "sum" => Some("Sum up this page in a few short lines."),
        "draft" => Some(
            "Draft a reply in this page's reply box: type it into the box with browser_act (fill), then stop. Do not press Post, Send or Submit — I will read it and send it myself. Answer with one short line saying where you put it.",
        ),
        _ => None,
    }
}

/// The JavaScript Undo runs on the drafted tab: empties the box the draft
/// went into (the focused field or editable), firing `input` so the page's
/// own state follows.
pub(crate) const UNDO_DRAFT_JS: &str = "(()=>{const e=document.activeElement;if(e&&'value' in e){e.value='';e.dispatchEvent(new Event('input',{bubbles:true}));return 'cleared'}if(e&&e.isContentEditable){e.textContent='';e.dispatchEvent(new Event('input',{bubbles:true}));return 'cleared'}return 'none'})()";

/// The answer an inline ask shows: the last assistant turn's text, once the
/// turn has settled. `None` while it runs.
pub(crate) fn settled_answer(session: &aui_protocol::Session, busy: bool) -> Option<String> {
    if busy {
        return None;
    }
    let turn = session.turns.iter().rev().find(|turn| matches!(turn, aui_protocol::Turn::Assistant { .. }))?;
    let text: Vec<&str> = turn
        .blocks()
        .iter()
        .filter_map(|block| match block {
            aui_protocol::Block::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let text = text.join("\n\n").trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// How many tool steps the last assistant turn took: past
/// [`INLINE_MAX_STEPS`] an inline ask grows into the card.
pub(crate) fn last_turn_steps(session: &aui_protocol::Session) -> usize {
    let Some(turn) = session.turns.iter().rev().find(|turn| matches!(turn, aui_protocol::Turn::Assistant { .. })) else {
        return 0;
    };
    turn.blocks()
        .iter()
        .map(|block| match block {
            aui_protocol::Block::ToolCall { .. } => 1,
            aui_protocol::Block::ToolGroup { calls, .. } => calls.len(),
            aui_protocol::Block::Activity { steps, .. } => steps.len(),
            _ => 0,
        })
        .sum()
}

/// The card's transcript for a browser chat's session: the person's asks
/// as bubbles (their page preamble dropped), each turn's tools as one steps
/// card, the answer as markdown, and an open question in the input's place.
pub(crate) fn agent_transcript(
    session: &aui_protocol::Session,
    title: &str,
    model: &str,
    busy: bool,
) -> aui::browse::agent_window::AgentTranscript {
    use aui::browse::agent_window::{AgentItem, AgentTranscript, Step, StepState};
    let mut items = Vec::new();
    let last = session.turns.len().saturating_sub(1);
    for (index, turn) in session.turns.iter().enumerate() {
        match turn {
            aui_protocol::Turn::User { text, .. } => items.push(AgentItem::User(strip_page_prompt(text).to_owned().into())),
            aui_protocol::Turn::Assistant { blocks, .. } => {
                let running = busy && index == last;
                let mut steps = Vec::new();
                let mut answer = String::new();
                for block in blocks {
                    match block {
                        aui_protocol::Block::ToolCall { verb, target, status, .. } => {
                            steps.push(Step::new(step_text(verb, target), step_state(status)));
                        }
                        aui_protocol::Block::ToolGroup { calls, .. } => {
                            for call in calls {
                                steps.push(Step::new(step_text(&call.verb, &call.target), step_state(&call.status)));
                            }
                        }
                        aui_protocol::Block::Text { text, .. } => {
                            if !answer.is_empty() {
                                answer.push_str("\n\n");
                            }
                            answer.push_str(text);
                        }
                        aui_protocol::Block::Question { header, .. } => {
                            items.push(AgentItem::Question { text: header.clone().into(), choices: Vec::new() });
                        }
                        _ => {}
                    }
                }
                if running {
                    if let Some(step) = steps.last_mut() {
                        if step.state == StepState::Done {
                            step.state = StepState::Running;
                        }
                    } else {
                        steps.push(Step::new("Thinking", StepState::Running));
                    }
                }
                if !steps.is_empty() {
                    items.push(AgentItem::Steps(steps));
                }
                if !answer.trim().is_empty() {
                    items.push(AgentItem::Answer(answer.into()));
                }
            }
        }
    }
    AgentTranscript { title: title.to_owned().into(), model: model.to_owned().into(), chips: Vec::new(), items }
}

/// A user turn without the page preamble [`page_prompt`] put in front; a
/// site action's ask reads as the action's own name.
pub(crate) fn strip_page_prompt(text: &str) -> &str {
    let rest = match text.strip_prefix("I'm looking at tab ").and_then(|_| text.split_once("\n\n")) {
        Some((_, rest)) => rest,
        None => text,
    };
    SITE_ACTIONS
        .iter()
        .find(|(id, _)| site_action_ask(id) == Some(rest))
        .map(|(_, label)| *label)
        .unwrap_or(rest)
}

fn step_text(verb: &str, target: &str) -> String {
    let target = target.trim();
    if target.is_empty() { verb.to_owned() } else { format!("{verb} {target}") }
}

fn step_state(status: &aui_protocol::ToolStatus) -> aui::browse::agent_window::StepState {
    use aui::browse::agent_window::StepState;
    match status {
        aui_protocol::ToolStatus::Running | aui_protocol::ToolStatus::Pending => StepState::Running,
        aui_protocol::ToolStatus::Error | aui_protocol::ToolStatus::Cancelled => StepState::Failed,
        _ => StepState::Done,
    }
}

impl Harness {
    /// ⌘J: open the line on the page on screen, or close it.
    pub(crate) fn toggle_agent_line(&mut self, cx: &mut Context<Self>) {
        if self.mode != crate::layout::Mode::Browse || self.browse_centre_shown().is_none() {
            return;
        }
        self.line_ui = if self.line_ui == LineUi::Closed { LineUi::Ask } else { LineUi::Closed };
        self.line_wants_keys = self.line_ui == LineUi::Ask;
        // "This page" stands again each time the line opens.
        if self.line_ui == LineUi::Ask {
            self.line_page_chip = true;
        }
        cx.notify();
    }

    /// Escape in the agent window: the handoff picker first, then the card,
    /// then the line's own state (never the driving bar's: Take over and
    /// the approval answer with their own buttons).
    pub(crate) fn escape_agent_window(&mut self, kind: crate::overlay::OverlayKind, cx: &mut Context<Self>) {
        if kind == crate::overlay::OverlayKind::AgentCard {
            if self.handoff_picker_open {
                self.handoff_picker_open = false;
            } else if self.card_ui == CardUi::Card {
                self.card_ui = CardUi::Closed;
            }
        } else {
            match self.line_ui {
                LineUi::Answer { .. } => {
                    self.line_ui = LineUi::Ask;
                    self.line_wants_keys = true;
                }
                LineUi::Ask | LineUi::Drafted { .. } => self.line_ui = LineUi::Closed,
                _ => {}
            }
        }
        cx.notify();
    }

    /// ⌘E: open the card on the current chat, or close it.
    pub(crate) fn toggle_agent_card(&mut self, cx: &mut Context<Self>) {
        if self.mode != crate::layout::Mode::Browse {
            return;
        }
        self.card_ui = if self.card_ui == CardUi::Closed { CardUi::Card } else { CardUi::Closed };
        // The card carries the answer on: the line does not show it twice.
        if self.card_ui != CardUi::Closed && matches!(self.line_ui, LineUi::Answer { .. }) {
            self.line_ui = LineUi::Closed;
        }
        cx.notify();
    }

    /// ⌘⇧E: dock the card as a column, or undock it.
    pub(crate) fn toggle_agent_column(&mut self, cx: &mut Context<Self>) {
        if self.mode != crate::layout::Mode::Browse {
            return;
        }
        self.card_ui = if self.card_ui == CardUi::Column { CardUi::Card } else { CardUi::Column };
        if matches!(self.line_ui, LineUi::Answer { .. }) {
            self.line_ui = LineUi::Closed;
        }
        cx.notify();
    }

    /// The tab on screen in Browse, as the chat's prompt names it.
    fn page_on_screen(&self, cx: &gpui::App) -> Option<(u64, String, String)> {
        let id = self.browse_centre_shown()?;
        let state = self.browse_centre_state()?;
        let page = state.read(cx);
        Some((id.0, page.title().to_string(), page.url().to_string()))
    }

    /// Send `ask` from the line (or the card's footer when `inline` is
    /// false): to the open chat when it is idle, else to a new one. The
    /// person's tab on screen is granted to the chat while the "This page"
    /// chip stands.
    pub(crate) fn ask_browser_chat(&mut self, ask: &str, inline: bool, draft: bool, window: &mut Window, cx: &mut Context<Self>) {
        let ask = ask.trim();
        if ask.is_empty() {
            return;
        }
        let page = if self.line_page_chip { self.page_on_screen(cx) } else { None };
        let prompt = page_prompt(ask, page.as_ref().map(|(tab, title, url)| (*tab, title.as_str(), url.as_str())));
        let reuse = self.browser_chat.clone().filter(|view| !view.read(cx).busy() && !view.read(cx).is_starting());
        let session = match reuse {
            Some(view) => {
                view.update(cx, |view, cx| view.send_text(prompt, cx));
                Some(view.read(cx).session_id.clone())
            }
            None => self.start_browser_chat(prompt, window, cx),
        };
        let Some(session) = session else {
            self.line_notice = Some("Connect a provider in Settings › Providers to ask about pages.".into());
            cx.notify();
            return;
        };
        if let Some((_, title, url)) = page.as_ref() {
            let pages = self.browser_chat_pages.entry(session.clone()).or_default();
            if !pages.iter().any(|(_, u)| u == url) {
                pages.push((title.clone(), url.clone()));
            }
        }
        if let (Some((tab, _, _)), Some(state)) = (page.as_ref(), self.browse_centre_state()) {
            let read_only = self.layout.agents.your_tabs == crate::browser::agents::YourTabs::ReadOnly;
            self.terminal_service.grant_tab(&session, *tab, state, read_only);
        }
        if let Some(input) = self.agent_line_input.clone() {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.line_notice = None;
        self.line_ui = match (draft, page) {
            (true, Some((tab, _, _))) => LineUi::Drafting { session, tab },
            _ if inline => LineUi::Answer { question: ask.to_owned(), session },
            _ => self.line_ui.clone(),
        };
        self.line_wants_keys = false;
        cx.notify();
    }

    /// Per frame, before the floats sync: an inline ask whose turn grew past
    /// [`INLINE_MAX_STEPS`] moves to the card; a draft whose turn settled
    /// lands as Drafted.
    pub(crate) fn settle_agent_line(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.browser_chat.clone() else { return };
        let (busy, id, steps) = {
            let view = view.read(cx);
            (view.busy() || view.is_starting(), view.session_id.clone(), view.transcript_session().map(last_turn_steps).unwrap_or(0))
        };
        match self.line_ui.clone() {
            LineUi::Answer { session, .. } if session == id && steps > INLINE_MAX_STEPS => {
                self.line_ui = LineUi::Closed;
                if self.card_ui == CardUi::Closed {
                    self.card_ui = CardUi::Card;
                }
                cx.notify();
            }
            LineUi::Drafting { session, tab } if session == id && !busy => {
                self.line_ui = LineUi::Drafted { session, tab };
                cx.notify();
            }
            _ => {}
        }
    }

    /// The line's own state (not the driving bar), for the float to draw.
    pub(crate) fn own_line_state(&self, cx: &gpui::App) -> Option<aui::browse::agent_window::LineState> {
        use aui::browse::agent_window::LineState;
        let chips = if self.line_page_chip { vec![page_chip()] } else { Vec::new() };
        match &self.line_ui {
            LineUi::Closed => None,
            LineUi::Ask => Some(LineState::Ask {
                text: "Ask about this page".into(),
                chips,
                site_actions: SITE_ACTIONS.iter().map(|(id, label)| ((*id).into(), (*label).into())).collect(),
            }),
            LineUi::Answer { question, session } => {
                let view = self.find_view(session, cx)?;
                let view = view.read(cx);
                let busy = view.busy() || view.is_starting();
                let answer = view.transcript_session().and_then(|s| settled_answer(s, busy));
                let title = self.page_on_screen(cx).map(|(_, title, _)| title);
                Some(LineState::Answer {
                    question: question.clone().into(),
                    answer: answer.map(Into::into),
                    source: title.filter(|t| !t.is_empty()).map(Into::into),
                    text: "Ask about this page".into(),
                    chips,
                })
            }
            // While the draft is typed the driving bar shows (the chat acts
            // on the tab); between its steps the line reads as working.
            LineUi::Drafting { .. } => Some(LineState::from_task(aui::browse::TaskFace::Typing, "Writing a draft in the page")),
            LineUi::Drafted { .. } => Some(LineState::Drafted { label: "Draft ready".into() }),
        }
    }

    /// One line intent that is not the driving bar's.
    pub(crate) fn own_line_intent(&mut self, intent: aui::browse::agent_window::LineIntent, window: &mut Window, cx: &mut Context<Self>) {
        use aui::browse::agent_window::LineIntent;
        match intent {
            LineIntent::Send(text) => self.ask_browser_chat(&text, true, false, window, cx),
            LineIntent::SiteAction(id) => {
                if let Some(ask) = site_action_ask(&id) {
                    self.ask_browser_chat(ask, true, id.as_ref() == "draft", window, cx);
                }
            }
            LineIntent::ChipRemove(_) => {
                self.line_page_chip = false;
                cx.notify();
            }
            LineIntent::Copy => {
                if let Some(text) = self.own_line_state(cx).and_then(|state| match state {
                    aui::browse::agent_window::LineState::Answer { answer, .. } => answer,
                    _ => None,
                }) {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.to_string()));
                }
            }
            LineIntent::Dismiss => {
                self.line_ui = LineUi::Ask;
                self.line_wants_keys = true;
                cx.notify();
            }
            LineIntent::Expand => {
                self.card_ui = CardUi::Card;
                if matches!(self.line_ui, LineUi::Answer { .. }) {
                    self.line_ui = LineUi::Closed;
                }
                cx.notify();
            }
            LineIntent::Undo => {
                if let LineUi::Drafted { tab, .. } = self.line_ui.clone() {
                    if let Some(state) = self.browser.states.get(&crate::browser::tabs::TabId(tab)).cloned() {
                        state.update(cx, |state, _| state.eval_with_result(u64::MAX - tab, UNDO_DRAFT_JS));
                    }
                }
                self.line_ui = LineUi::Closed;
                cx.notify();
            }
            LineIntent::Done => {
                self.line_ui = LineUi::Closed;
                cx.notify();
            }
            LineIntent::Stop => {
                if let Some(view) = self.browser_chat.clone() {
                    view.update(cx, |view, cx| view.interrupt(cx));
                }
                self.line_ui = LineUi::Ask;
                cx.notify();
            }
            LineIntent::Plus => {}
            _ => {}
        }
    }
}

impl Harness {
    /// One card (or column) intent.
    pub(crate) fn card_intent(&mut self, intent: aui::browse::agent_window::CardIntent, window: &mut Window, cx: &mut Context<Self>) {
        use aui::browse::agent_window::CardIntent;
        match intent {
            CardIntent::Send(text) | CardIntent::Queue(text) => {
                self.ask_browser_chat(&text, false, false, window, cx);
                if let Some(input) = self.agent_card_input.clone() {
                    input.update(cx, |input, cx| input.set_value("", window, cx));
                }
            }
            CardIntent::Dock => self.card_ui = CardUi::Column,
            CardIntent::Undock => self.card_ui = CardUi::Card,
            CardIntent::Close => {
                self.card_ui = CardUi::Closed;
                self.handoff_picker_open = false;
            }
            CardIntent::ChipRemove(_) => self.line_page_chip = false,
            CardIntent::Model => self.open_settings_page("browser", cx),
            CardIntent::Handoff => {
                self.handoff_picker_open = !self.handoff_picker_open;
                self.handoff_selected = 0;
                if let Some(query) = self.handoff_query.clone() {
                    query.update(cx, |query, cx| query.set_value("", window, cx));
                }
            }
            CardIntent::OpenHandoff => self.open_handoff_target(window, cx),
            CardIntent::Choose(_) | CardIntent::Plus => {}
        }
        cx.notify();
    }
}

impl Harness {
    /// `browser-chat-fixture:<answer|steps|draft>` (free: no turn, no wire):
    /// a read-only browser chat whose transcript is canned, made the line's
    /// chat. `answer` lands the line in its inline answer, `steps` opens the
    /// card on a multi-step chat, `draft` lands the draft state.
    pub(crate) fn step_browser_chat_fixture(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        use aui_protocol::{Block, Session, ToolBody, ToolKind, ToolStatus, Turn, TurnMeta};
        let kind = rest.trim();
        let id = format!("browser-fixture-{}", if kind.is_empty() { "answer" } else { kind });
        crate::baaz_log!("browser-chat-fixture: {id} (page {:?})", self.browse_centre_shown());
        let page = self.page_on_screen(cx);
        let ask = match kind {
            "steps" => "Compare the refund windows on these three shops and save the policy pages as PDFs to ~/Documents/refunds",
            "draft" => site_action_ask("draft").unwrap_or_default(),
            _ => "What's the refund window?",
        };
        let prompt = page_prompt(ask, page.as_ref().map(|(tab, title, url)| (*tab, title.as_str(), url.as_str())));
        let call = |verb: &str, target: &str| Block::ToolCall {
            id: format!("{verb}-{target}"),
            kind: ToolKind::Browser,
            verb: verb.into(),
            target: target.into(),
            status: ToolStatus::Success,
            duration_ms: Some(400),
            body: ToolBody::None,
            diff_stat: None,
        };
        let blocks = match kind {
            "steps" => vec![
                call("Read", "shop.example/returns"),
                call("Opened", "northgear.example/refunds"),
                call("Read", "northgear.example/refunds"),
                call("Opened", "lumen.example/policy"),
                call("Saved", "~/Documents/refunds/lumen.pdf"),
                Block::Text {
                    text: "**shop.example** gives 30 days, **northgear.example** 14, and **lumen.example** 60 for unopened items.\n\n- Three PDFs are in `~/Documents/refunds`.".into(),
                    streaming: false,
                },
            ],
            "draft" => vec![call("Typed into", "Your reply"), Block::Text { text: "The draft is in \u{201C}Your reply\u{201D}.".into(), streaming: false }],
            _ => vec![
                call("Read", "this page"),
                Block::Text {
                    text: "Within 30 days of delivery. Final-sale items, gift cards and opened software can't be returned; the refund reaches your card 5–7 business days after the return arrives.".into(),
                    streaming: false,
                },
            ],
        };
        let mut session = Session::new(id.clone(), aui_protocol::Provider::Claude, "haiku", std::env::temp_dir().to_string_lossy());
        session.turns = vec![
            Turn::User { id: "u1".into(), text: prompt, attachments: Vec::new(), mentions: Vec::new(), timestamp: None },
            Turn::Assistant { id: "a1".into(), blocks, meta: TurnMeta::default(), timestamp: None },
        ];
        let host = crate::session::SessionHost {
            provider_id: "claude-code".into(),
            workspace: std::env::temp_dir().to_string_lossy().into_owned(),
            overlays: self.overlays.clone(),
            capture: self.capture.clone(),
            terminal_host: Some(self.terminal_host.clone()),
        };
        // Reachable over the socket like a real chat, so a check can make it
        // open tabs and work (its task folder, Done closes tabs) — no turn.
        self.terminal_service.register_session(&id, std::env::temp_dir());
        let view = cx.new(|cx| crate::session::SessionView::new(id.clone(), host, window, cx));
        view.update(cx, |view, cx| view.load_fixture(session, cx));
        self.set_override(&id, |meta| {
            meta.hidden = true;
            meta.browser_chat = true;
        }, cx);
        if let Some((_, title, url)) = page.as_ref() {
            self.browser_chat_pages.insert(id.clone(), vec![(title.clone(), url.clone())]);
        }
        self.browser_chat = Some(view);
        match kind {
            "steps" => self.card_ui = CardUi::Card,
            "draft" => {
                if let Some((tab, _, _)) = page {
                    self.line_ui = LineUi::Drafted { session: id, tab };
                }
            }
            _ => self.line_ui = LineUi::Answer { question: ask.to_owned(), session: id },
        }
        cx.notify();
    }
}

/// How long a done session keeps its tabs (docs/27 §4.3: "Done closes them
/// after 15 min unless you drag a tab up into your own list").
pub(crate) const DONE_CLOSES_AFTER_MS: u64 = 15 * 60 * 1000;

/// Which of `sessions` (id, working now) are due to lose their tabs at
/// `now`, updating `seen` (sessions this run saw working) and `done_at`
/// (when each went idle). Only a session seen working and then idle past
/// [`DONE_CLOSES_AFTER_MS`] is due; working again clears its clock.
pub(crate) fn done_sessions_due(
    sessions: &[(String, bool)],
    now: u64,
    after: u64,
    seen: &mut std::collections::HashSet<String>,
    done_at: &mut std::collections::HashMap<String, u64>,
) -> Vec<String> {
    let mut due = Vec::new();
    for (session, working) in sessions {
        if *working {
            seen.insert(session.clone());
            done_at.remove(session);
            continue;
        }
        if !seen.contains(session) {
            continue;
        }
        let since = *done_at.entry(session.clone()).or_insert(now);
        if now.saturating_sub(since) >= after {
            due.push(session.clone());
        }
    }
    due
}

impl Harness {
    /// Close the tabs of task folders done for [`DONE_CLOSES_AFTER_MS`]
    /// (docs/27 §4.3). A tab the person dragged into their own list is
    /// theirs and stays; the tab on screen stays until they leave it.
    pub(crate) fn close_done_task_tabs(&mut self, shown: Option<crate::browser::tabs::TabId>, window: &Window, cx: &mut Context<Self>) {
        let sessions: Vec<(String, bool)> = self
            .browser
            .tabs
            .session_tabs(None)
            .iter()
            .map(|(session, _)| {
                let entry = self.sessions.iter().find(|e| &e.id == session);
                let view_busy = self.find_view(session, cx).is_some_and(|v| v.read(cx).busy());
                let working = entry.is_some_and(|e| e.running) || view_busy || self.terminal_service.agent_activity(session).is_some();
                (session.clone(), working)
            })
            .collect();
        let now = crate::browser::tabs::now_ms();
        // `BAAZ_DONE_CLOSE_SECS` shortens the wait for a live check.
        let after = std::env::var("BAAZ_DONE_CLOSE_SECS").ok().and_then(|v| v.parse::<u64>().ok()).map(|s| s * 1000).unwrap_or(DONE_CLOSES_AFTER_MS);
        let due = done_sessions_due(&sessions, now, after, &mut self.task_seen_working, &mut self.task_done_at);
        // An idle window draws no frames, so the sweep would never run again:
        // wake it when the next done folder comes due.
        let next = self.task_done_at.values().map(|since| (since + after).saturating_sub(now)).min();
        if let Some(wait) = next.filter(|_| self.task_done_timer.is_none()) {
            self.task_done_timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(std::time::Duration::from_millis(wait + 50)).await;
                let _ = this.update(cx, |this, cx| {
                    this.task_done_timer = None;
                    cx.notify();
                });
            }));
        }
        for session in due {
            let owner = crate::browser::Owner::Session(session.clone());
            let ids: Vec<_> = self.browser.tabs.tabs_of(&owner).iter().map(|tab| tab.id).filter(|id| Some(*id) != shown).collect();
            if ids.is_empty() {
                continue;
            }
            crate::baaz_log!("done task folder closes {} tab(s) session={session}", ids.len());
            self.task_done_at.remove(&session);
            self.task_seen_working.remove(&session);
            // The sweep runs inside the frame's sync: close on the next turn.
            cx.defer_in(window, move |this, window, cx| this.close_tabs(ids, window, cx));
        }
    }
}

/// The "This page" chip.
fn page_chip() -> aui::composer::ComposerChip {
    aui::composer::ComposerChip {
        id: "this-page".into(),
        kind: aui::composer::ComposerChipKind::File,
        label: "This page".into(),
        removable: true,
        thumbnail: None,
        detail: None,
        state: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_preamble_names_the_tab_and_strips_back_off() {
        let prompt = page_prompt("what's the refund window?", Some((7, "Returns", "https://shop.example/returns")));
        assert!(prompt.contains("tab=7"));
        assert_eq!(strip_page_prompt(&prompt), "what's the refund window?");
        assert_eq!(page_prompt("hi", None), "hi");
        assert_eq!(strip_page_prompt("plain ask"), "plain ask");
        let draft = page_prompt(site_action_ask("draft").unwrap(), Some((1, "t", "u")));
        assert_eq!(strip_page_prompt(&draft), "Draft a reply");
    }

    fn done_sessions_due_t(
        sessions: &[(String, bool)],
        now: u64,
        seen: &mut std::collections::HashSet<String>,
        done: &mut std::collections::HashMap<String, u64>,
    ) -> Vec<String> {
        done_sessions_due(sessions, now, DONE_CLOSES_AFTER_MS, seen, done)
    }

    #[test]
    fn only_a_session_seen_working_closes_its_tabs_after_15_minutes() {
        let mut seen = std::collections::HashSet::new();
        let mut done = std::collections::HashMap::new();
        let idle = |id: &str| (id.to_owned(), false);
        // Never seen working: never due.
        assert!(done_sessions_due_t(&[idle("a")], 0, &mut seen, &mut done).is_empty());
        assert!(done_sessions_due_t(&[idle("a")], DONE_CLOSES_AFTER_MS * 2, &mut seen, &mut done).is_empty());
        // Working, then idle: due 15 minutes after it went idle.
        assert!(done_sessions_due_t(&[("b".into(), true)], 1_000, &mut seen, &mut done).is_empty());
        assert!(done_sessions_due_t(&[idle("b")], 2_000, &mut seen, &mut done).is_empty());
        assert!(done_sessions_due_t(&[idle("b")], 2_000 + DONE_CLOSES_AFTER_MS - 1, &mut seen, &mut done).is_empty());
        assert_eq!(done_sessions_due_t(&[idle("b")], 2_000 + DONE_CLOSES_AFTER_MS, &mut seen, &mut done), vec!["b".to_owned()]);
        // Working again clears the clock.
        assert!(done_sessions_due_t(&[("b".into(), true)], 5_000_000, &mut seen, &mut done).is_empty());
        assert!(done_sessions_due_t(&[idle("b")], 5_000_001, &mut seen, &mut done).is_empty());
    }

    #[test]
    fn the_draft_action_never_sends() {
        let ask = site_action_ask("draft").unwrap();
        assert!(ask.contains("Do not press"));
        assert!(site_action_ask("nope").is_none());
    }
}
