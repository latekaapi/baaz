//! The agent window over a Browse page (docs/27 §4.5, R3): the ⌘E card and
//! the ⌘⇧E column, wired to a browser chat ([`super::browser_chat`]). The
//! library draws every state (agentic-ui `browse/agent-card`,
//! `browse/agent-column`, `browse/handoff`, and the driving line); this
//! module decides which one shows and turns their intents into Baaz actions.
//!
//! - **The card** is the one chat (owner 2026-11-28): every way to ask —
//!   ⌘J, the ✦ button, the Field's Ask — opens it on the page on screen.
//!   While its body is empty it offers the quick actions ("Sum it up",
//!   "Draft a reply"); "Draft a reply" writes into the page's own box and
//!   never sends. **The column** is the same card docked beside the page.
//! - While the chat (or any session) acts on the tab on screen, the line
//!   over the page's foot is the driving bar (R2): Take over, the page's
//!   approvals. The line has no chat of its own.

#[cfg(feature = "probe")]
use gpui::AppContext as _;
use gpui::{Context, Window};

use super::Harness;
use crate::layout::LayoutGlobal as _;

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

/// The agent window's own state (task 4.5): where the card stands, its
/// "This page" chip and float window, the driving bar's input, and what
/// each browser chat used and was handed off to. One field of the Browse
/// view it floats over ([`crate::browser::BrowserView::agent_window`],
/// task 4.5b).
pub(crate) struct AgentWindowState {
    /// Where the agent card stands: closed, floating, or docked.
    pub(crate) card_ui: CardUi,
    /// The card's "This page" chip stands (× removes it, `+` or the next
    /// open puts it back).
    pub(crate) page_chip: bool,
    /// One quiet sentence the card shows instead of an answer (no provider).
    pub(crate) notice: Option<String>,
    /// The card's steering input.
    pub(crate) agent_card_input: Option<gpui::Entity<gpui_kit::base::input::TextareaState>>,
    /// The card's float window, while it is open or opening.
    pub(crate) card_float: Option<crate::overlay::Overlay>,
    /// Browser chats handed off (R3 board 3b), by chat id: the target
    /// session's id, its label and the card row's detail line.
    pub(crate) browser_handoffs: std::collections::HashMap<String, (String, String, String)>,
    /// The person's pages each browser chat was asked about ("This page"):
    /// `(title, url)` by chat id, for the handoff's "Pages it used".
    pub(crate) browser_chat_pages: std::collections::HashMap<String, Vec<(String, String)>>,
    /// Provider prompts a browser chat already accepted for its own
    /// browser tools (each once).
    pub(crate) auto_accepted: std::collections::HashSet<String>,
    /// The driving bar's input: the library's line takes one even though
    /// the bar only reports.
    pub(crate) agent_line_input: Option<gpui::Entity<gpui_kit::base::input::TextareaState>>,
}

impl Default for AgentWindowState {
    fn default() -> Self {
        Self {
            card_ui: CardUi::Closed,
            page_chip: true,
            notice: None,
            agent_card_input: None,
            card_float: None,
            browser_handoffs: std::collections::HashMap::new(),
            browser_chat_pages: std::collections::HashMap::new(),
            auto_accepted: std::collections::HashSet::new(),
            agent_line_input: None,
        }
    }
}

/// The column's width, beside the page.
pub(crate) const COLUMN_W: f32 = 400.0;

/// The card's quick actions, offered while its body is empty (docs/27
/// §4.5; the line's site actions until owner 2026-11-28).
pub(crate) const QUICK_ACTIONS: [(&str, &str); 2] = [("sum", "Sum it up"), ("draft", "Draft a reply")];

/// [`QUICK_ACTIONS`] as the library's card takes them.
pub(crate) fn quick_actions() -> Vec<(gpui::SharedString, gpui::SharedString)> {
    QUICK_ACTIONS.iter().map(|(id, label)| ((*id).into(), (*label).into())).collect()
}

/// The prompt that tells a browser chat which page the person means, ahead
/// of their words. The tab is theirs: the grant lets the chat use it.
pub(crate) fn page_prompt(ask: &str, page: Option<(u64, &str, &str)>) -> String {
    // Browse where the person can see it: Baaz's own tabs land in this
    // chat's task folder; WebFetch and WebSearch pages are never shown
    // (seen live: Haiku reached for WebFetch on a research ask).
    let how = "Browse with the Baaz browser tools (browser_tabs to open each site in its own tab, then browser_read) so I can see the pages; don't use WebFetch or WebSearch.";
    match page {
        Some((tab, title, url)) => format!(
            "I'm looking at tab {tab} in my browser: \u{201C}{title}\u{201D} ({url}). Use the browser tools with tab={tab} to read it. {how}\n\n{ask}"
        ),
        None => format!("{how}\n\n{ask}"),
    }
}

/// A quick action's ask, in the person's words.
pub(crate) fn site_action_ask(id: &str) -> Option<&'static str> {
    match id {
        "sum" => Some("Sum up this page in a few short lines."),
        "draft" => Some(
            "Draft a reply in this page's reply box: type it into the box with browser_act (fill), then stop. Do not press Post, Send or Submit — I will read it and send it myself. Answer with one short line saying where you put it.",
        ),
        _ => None,
    }
}

/// A browser chat's answer: the last assistant turn's text after its last
/// tool call, once the turn has settled. `None` while it runs.
pub(crate) fn settled_answer(session: &aui_protocol::Session, busy: bool) -> Option<String> {
    if busy {
        return None;
    }
    let turn = session.turns.iter().rev().find(|turn| matches!(turn, aui_protocol::Turn::Assistant { .. }))?;
    // The answer is what follows the turn's last tool call; earlier text is
    // narration (the card folds it into the steps; seen live: a handoff draft
    // carried "Let me try…" lines).
    let blocks = turn.blocks();
    let after = blocks
        .iter()
        .rposition(|b| matches!(b, aui_protocol::Block::ToolCall { .. } | aui_protocol::Block::ToolGroup { .. }))
        .map(|at| at + 1)
        .unwrap_or(0);
    let text: Vec<&str> = blocks[after..]
        .iter()
        .filter_map(|block| match block {
            aui_protocol::Block::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let text = text.join("\n\n").trim().to_owned();
    (!text.is_empty()).then_some(text)
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
                // Text written before the turn's last tool call is narration
                // ("Let me read the page…"): it folds into the steps, and only
                // what follows the last call is the answer (docs/27 §4.5:
                // steps collapse above the answer).
                let last_tool = blocks.iter().rposition(|b| matches!(b, aui_protocol::Block::ToolCall { .. } | aui_protocol::Block::ToolGroup { .. }));
                for (at, block) in blocks.iter().enumerate() {
                    if let (aui_protocol::Block::Text { text, .. }, Some(last_tool)) = (block, last_tool) {
                        if at < last_tool {
                            let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
                            if !line.is_empty() {
                                steps.push(Step::new(line.to_owned(), StepState::Done));
                            }
                            continue;
                        }
                    }
                    match block {
                        aui_protocol::Block::ToolCall { verb, target, status, .. } => {
                            steps.push(Step::new(agent_step_text(verb, target), agent_step_state(status)));
                        }
                        aui_protocol::Block::ToolGroup { calls, .. } => {
                            for call in calls {
                                steps.push(Step::new(agent_step_text(&call.verb, &call.target), agent_step_state(&call.status)));
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
    AgentTranscript { title: title.to_owned().into(), model: model.to_owned().into(), chips: Vec::new(), items, actions: quick_actions(), page_on_screen: false }
}

/// A user turn without the page preamble [`page_prompt`] put in front; a
/// quick action's ask reads as the action's own name.
pub(crate) fn strip_page_prompt(text: &str) -> &str {
    let preamble = text.starts_with("I'm looking at tab ") || text.starts_with("Browse with the Baaz browser tools");
    let rest = match preamble.then(|| text.split_once("\n\n")).flatten() {
        Some((_, rest)) => rest,
        None => text,
    };
    QUICK_ACTIONS
        .iter()
        .find(|(id, _)| site_action_ask(id) == Some(rest))
        .map(|(_, label)| *label)
        .unwrap_or(rest)
}

/// One step row: the verb and its target on one line, short — a long shell
/// command wrapped over the rows below it (seen live 8 Nov).
fn agent_step_text(verb: &str, target: &str) -> String {
    let target = target.lines().next().unwrap_or("").trim();
    let text = if target.is_empty() { verb.to_owned() } else { format!("{verb} {target}") };
    // One line in the steps card's narrow header, whatever the command.
    if text.chars().count() > 48 { format!("{}…", text.chars().take(47).collect::<String>()) } else { text }
}

/// What the newest waiting tool call would do, one line: the detail an
/// approval question shows when the provider's own card carries none
/// (Claude Code's Bash card names only the tool).
pub(crate) fn pending_call_detail(session: &aui_protocol::Session) -> Option<String> {
    session.turns.iter().rev().flat_map(|turn| turn.blocks().iter().rev()).find_map(|block| match block {
        aui_protocol::Block::ToolCall { target, status: aui_protocol::ToolStatus::Pending | aui_protocol::ToolStatus::Running, .. }
            if !target.trim().is_empty() =>
        {
            let line = target.lines().next().unwrap_or("").trim().to_owned();
            Some(if line.chars().count() > 160 { format!("{}…", line.chars().take(159).collect::<String>()) } else { line })
        }
        _ => None,
    })
}

fn agent_step_state(status: &aui_protocol::ToolStatus) -> aui::browse::agent_window::StepState {
    use aui::browse::agent_window::StepState;
    match status {
        aui_protocol::ToolStatus::Running | aui_protocol::ToolStatus::Pending => StepState::Running,
        aui_protocol::ToolStatus::Error | aui_protocol::ToolStatus::Cancelled => StepState::Failed,
        _ => StepState::Done,
    }
}

impl crate::browser::BrowserView {
    /// Where the agent card stands; the float follows from this view's
    /// notify.
    pub(crate) fn set_agent_card(&mut self, card: CardUi, cx: &mut Context<Self>) {
        self.agent_window.card_ui = card;
        cx.notify();
    }

    /// Stand the "This page" chip for the next ask, or drop it.
    pub(crate) fn set_page_chip(&mut self, on: bool, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.agent_window.page_chip, on) != on {
            cx.notify();
        }
    }

    /// The card's notice in its body (a missing provider), or none.
    pub(crate) fn set_agent_notice(&mut self, notice: Option<String>, cx: &mut Context<Self>) {
        if self.agent_window.notice != notice {
            self.agent_window.notice = notice;
            cx.notify();
        }
    }
}

impl Harness {
    /// ⌘J and ⌘E: open the card on the page on screen
    /// ([`Self::open_agent_card`]), or close it (floating or docked).
    pub(crate) fn toggle_agent_card(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if crate::layout::Layout::get(cx).mode != crate::layout::Mode::Browse {
            return;
        }
        if self.browser.read(cx).agent_window.card_ui == CardUi::Closed {
            self.open_agent_card(window, cx);
        } else {
            self.browser.update(cx, |browser, cx| browser.set_agent_card(CardUi::Closed, cx));
        }
    }

    /// The one Ask (docs/82 8.2, owner 2026-11-28): ⌘J, ⌘E, the ✦ button
    /// and the Field's Ask all open the card here, on the page shown, ready
    /// to type. "This page" stands again each time — unless no page is
    /// shown (a new tab), where the card asks with no page. A card already
    /// open (floating or docked) stays where it is and takes the keyboard.
    /// Browse only; with no tab at all, Browse's own blank tab opens first.
    pub(crate) fn open_agent_card(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if crate::layout::Layout::get(cx).mode != crate::layout::Mode::Browse {
            return;
        }
        if self.browser.read(cx).browse_centre_shown(cx).is_none() {
            self.ensure_browse_tab(window, cx);
        }
        let page = self.real_page_shown(cx);
        let open = self.browser.read(cx).agent_window.card_ui != CardUi::Closed;
        self.browser.update(cx, |browser, cx| {
            browser.set_page_chip(page, cx);
            if !open {
                browser.set_agent_card(CardUi::Card, cx);
            }
        });
        // A card opening takes the keyboard as its window opens
        // (`sync_card_float`); one already open takes it here.
        if open {
            self.focus_agent_card(cx);
        }
    }

    /// Whether the tab on screen shows a real page to ask about (not a new
    /// tab): the "This page" chip's rule.
    pub(crate) fn real_page_shown(&self, cx: &gpui::App) -> bool {
        self.page_on_screen(cx).is_some_and(|(_, _, url)| !url.is_empty() && url != crate::browser::tabs::BLANK) && !self.browse_centre_tab_blank(cx)
    }

    /// Escape in the card's window: the handoff picker first, then the
    /// floating card (a docked column stays). The driving bar never closes
    /// on Escape: Take over and the approval answer with their own buttons.
    pub(crate) fn escape_agent_window(&mut self, kind: crate::overlay::OverlayKind, cx: &mut Context<Self>) {
        if kind != crate::overlay::OverlayKind::AgentCard {
            return;
        }
        if let Some(picker) = self.handoff_picker_view(cx) {
            // The picker's own dismissal, through the overlay stack.
            picker.update(cx, |picker, cx| picker.dismiss(cx));
        } else if self.browser.read(cx).agent_window.card_ui == CardUi::Card {
            self.browser.update(cx, |browser, cx| browser.set_agent_card(CardUi::Closed, cx));
        }
    }

    /// ⌘⇧E: dock the card as a column, or undock it.
    pub(crate) fn toggle_agent_column(&mut self, cx: &mut Context<Self>) {
        if crate::layout::Layout::get(cx).mode != crate::layout::Mode::Browse {
            return;
        }
        self.browser.update(cx, |browser, cx| {
            let card = if browser.agent_window.card_ui == CardUi::Column { CardUi::Card } else { CardUi::Column };
            browser.set_agent_card(card, cx);
        });
    }

    /// The tab on screen in Browse, as the chat's prompt names it.
    fn page_on_screen(&self, cx: &gpui::App) -> Option<(u64, String, String)> {
        let id = self.browser.read(cx).browse_centre_shown(cx)?;
        let state = self.browse_centre_state(cx)?;
        let page = state.read(cx);
        Some((id.0, page.title().to_string(), page.url().to_string()))
    }

    /// Send `ask` as a new ask in the browser chat: to the open chat when
    /// it is idle, else to a new one. The person's tab on screen is granted
    /// to the chat while the "This page" chip stands.
    pub(crate) fn ask_browser_chat(&mut self, ask: &str, window: &mut Window, cx: &mut Context<Self>) {
        let ask = ask.trim();
        if ask.is_empty() {
            return;
        }
        let page = if self.browser.read(cx).agent_window.page_chip { self.page_on_screen(cx) } else { None };
        let prompt = page_prompt(ask, page.as_ref().map(|(tab, title, url)| (*tab, title.as_str(), url.as_str())));
        // The open chat continues while it is idle and still on the
        // Assistant's provider; after a change in Settings the next ask
        // starts a fresh chat there (seen live: a Muse chat kept taking
        // asks after Claude Haiku was picked).
        let provider = self.assistant_choice(cx).map(|choice| choice.provider);
        let reuse = self
            .registry
            .browser_chat()
            .cloned()
            .filter(|view| !view.read(cx).rt(cx).busy() && !view.read(cx).rt(cx).is_starting())
            .filter(|view| Some(view.read(cx).rt(cx).provider_kind()) == provider);
        let session = match reuse {
            Some(view) => {
                view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.send_or_queue(prompt, cx)));
                Some(view.read(cx).rt(cx).session_id.clone())
            }
            None => self.start_browser_chat(prompt, window, cx),
        };
        let Some(session) = session else {
            self.browser.update(cx, |browser, cx| browser.set_agent_notice(Some("Connect a provider in Settings › Providers to ask about pages.".into()), cx));
            return;
        };
        if let Some((_, title, url)) = page.as_ref() {
            self.browser.update(cx, |browser, _| {
                let pages = browser.agent_window.browser_chat_pages.entry(session.clone()).or_default();
                if !pages.iter().any(|(_, u)| u == url) {
                    pages.push((title.clone(), url.clone()));
                }
            });
        }
        if let (Some((tab, _, _)), Some(state)) = (page.as_ref(), self.browse_centre_state(cx)) {
            let read_only = crate::layout::Layout::get(cx).agents.your_tabs == crate::browser::agents::YourTabs::ReadOnly;
            self.terminal_service.grant_tab(&session, *tab, state, read_only);
        }
        self.browser.update(cx, |browser, cx| browser.set_agent_notice(None, cx));
    }

    /// A browser chat's own browser tools need no second ask from its
    /// provider: Baaz's page gate (docs/27 §6.2, Settings › Browser ›
    /// Agents) already stops Send, Pay, Delete, Upload and sign-in, and any
    /// action under Ask. So the provider's prompt for `mcp__baaz__browser_*`
    /// is accepted here, once per prompt; every other tool (files, shell)
    /// still asks in the card. Runs on the browser chat's own changes and
    /// every settle, before the floats follow (`Harness::watch_browser_chat`).
    pub(crate) fn auto_accept_browser_tools(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.registry.browser_chat().cloned() else { return };
        let due: Vec<String> = view
            .read(cx)
            .rt(cx).pending_external_approvals()
            .into_iter()
            .filter(|(id, headline)| headline.contains("mcp__baaz__browser_") && !self.browser.read(cx).agent_window.auto_accepted.contains(id))
            .map(|(id, _)| id)
            .collect();
        for id in due {
            self.browser.update(cx, |browser, _| browser.agent_window.auto_accepted.insert(id.clone()));
            view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.answer_external(&id, true, cx)));
        }
    }
}

impl Harness {
    /// One card (or column) intent.
    pub(crate) fn card_intent(&mut self, intent: aui::browse::agent_window::CardIntent, window: &mut Window, cx: &mut Context<Self>) {
        use aui::browse::agent_window::CardIntent;
        let queue = matches!(intent, CardIntent::Queue(_));
        match intent {
            // The card talks to its own chat: ↩ steers the running turn (or
            // sends the next), ⌘↩ queues behind it. Only with no chat yet does
            // it start one (seen live: a steer through the ask's reuse rule
            // started a second chat).
            CardIntent::Send(text) | CardIntent::Queue(text) => {
                match self.registry.browser_chat().cloned() {
                    Some(view) => view.update(cx, |view, cx| {
                        if queue {
                            view.runtime().clone().update(cx, |rt, cx| rt.send_or_queue(text, cx));
                        } else {
                            view.runtime().clone().update(cx, |rt, cx| rt.steer_or_send(text, cx));
                        }
                    }),
                    None => self.ask_browser_chat(&text, window, cx),
                }
                if let Some(input) = self.browser.read(cx).agent_window.agent_card_input.clone() {
                    input.update(cx, |input, cx| input.set_value("", window, cx));
                }
            }
            // A quick action of the empty card: its ask, as the person's
            // own. "Draft a reply" writes into the page's box and never
            // sends — its prompt says so.
            CardIntent::Quick(id) => {
                if let Some(ask) = site_action_ask(&id) {
                    self.ask_browser_chat(ask, window, cx);
                }
            }
            CardIntent::Dock => self.browser.update(cx, |browser, cx| browser.set_agent_card(CardUi::Column, cx)),
            CardIntent::Undock => self.browser.update(cx, |browser, cx| browser.set_agent_card(CardUi::Card, cx)),
            CardIntent::Close => {
                self.browser.update(cx, |browser, cx| browser.set_agent_card(CardUi::Closed, cx));
                self.overlays.update(cx, |o, cx| { o.close(crate::overlay_stack::LayerKind::HandoffPicker, cx); });
            }
            CardIntent::ChipRemove(_) => self.browser.update(cx, |browser, cx| browser.set_page_chip(false, cx)),
            // `+` (shown only while no chip stands): "This page" back, when
            // there is a page to ask about.
            CardIntent::Plus => {
                if self.real_page_shown(cx) {
                    self.browser.update(cx, |browser, cx| browser.set_page_chip(true, cx));
                }
            }
            CardIntent::Model => self.open_settings_page("browser", cx),
            CardIntent::Handoff => self.toggle_handoff_picker(cx),
            CardIntent::OpenHandoff => self.open_handoff_target(window, cx),
            // The card's question block: a pending approval's Allow (0) or
            // Don't allow (1).
            CardIntent::Choose(index) => {
                if let Some(view) = self.registry.browser_chat().cloned() {
                    view.update(cx, |view, cx| {
                        // The question names the newest provider approval:
                        // answer that one, by id.
                        match view.rt(cx).pending_external_approvals().last().map(|(id, _)| id.clone()) {
                            Some(id) => view.runtime().clone().update(cx, |rt, cx| rt.answer_external(&id, index == 0, cx)),
                            None if index == 0 => {
                                view.allow_pending(window, cx);
                            }
                            None => {
                                view.deny_pending(window, cx);
                            }
                        }
                    });
                }
            }
        }
    }
}

impl Harness {
    #[cfg(feature = "probe")]
    /// `browser-chat-fixture:<answer|steps|draft>` (free: no turn, no wire):
    /// a read-only browser chat whose transcript is canned, made the card's
    /// chat, and the card opened on it: `answer` a one-step answer, `steps`
    /// a multi-step chat, `draft` a "Draft a reply" that landed; empty is
    /// `answer`. Any other payload fails the step.
    pub(crate) fn step_browser_chat_fixture(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        use aui_protocol::{Block, Session, ToolBody, ToolKind, ToolStatus, Turn, TurnMeta};
        let kind = rest.trim();
        if !matches!(kind, "" | "answer" | "steps" | "draft") {
            return Err(format!("unknown fixture `{kind}`; known fixtures are answer, steps, draft"));
        }
        let id = format!("browser-fixture-{}", if kind.is_empty() { "answer" } else { kind });
        crate::baaz_log!("browser-chat-fixture: {id} (page {:?})", self.browser.read(cx).browse_centre_shown(cx));
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
            terminal_host: Some(self.terminal_host(cx)),
        };
        // Reachable over the socket like a real chat, so a check can make it
        // open tabs and work (its task folder, Done closes tabs) — no turn.
        self.terminal_service.register_session(&id, std::env::temp_dir());
        let view = cx.new(|cx| crate::session::SessionView::new(id.clone(), host, window, cx));
        view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.load_fixture(session, cx)));
        self.set_override(&id, |meta| {
            meta.hidden = true;
            meta.browser_chat = true;
        }, cx);
        if let Some((_, title, url)) = page.as_ref() {
            self.browser.update(cx, |browser, _| browser.agent_window.browser_chat_pages.insert(id.clone(), vec![(title.clone(), url.clone())]));
        }
        self.set_browser_chat(view, cx);
        self.browser.update(cx, |browser, cx| {
            if browser.agent_window.card_ui == CardUi::Closed {
                browser.set_agent_card(CardUi::Card, cx);
            }
        });
        Ok(())
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
    /// theirs and stays; the tab on screen stays until they leave it. Runs
    /// on the Browse view's page reconcile, on every session event (which
    /// is where a session starts and stops working), and on the view's own
    /// timer when the next folder comes due ([`BrowserEvent::DoneTasksDue`](crate::browser::BrowserEvent)).
    pub(crate) fn close_done_task_tabs(&mut self, cx: &mut Context<Self>) {
        if !self.browser.read(cx).tabs_loaded {
            return;
        }
        let shown = self.browser.read(cx).occlusion.shown().on_screen();
        let sessions: Vec<(String, bool)> = self
            .browser
            .read(cx)
            .tabs
            .session_tabs(None)
            .iter()
            .map(|(session, _)| {
                let entry = self.session_list.read(cx).sessions.iter().find(|e| &e.id == session);
                let view_busy = self.find_view(session, cx).is_some_and(|v| v.read(cx).rt(cx).busy());
                let working = entry.is_some_and(|e| e.running) || view_busy || self.terminal_service.agent_activity(session).is_some();
                (session.clone(), working)
            })
            .collect();
        let now = crate::browser::tabs::now_ms();
        // `BAAZ_DONE_CLOSE_SECS` shortens the wait for a live check.
        let after = std::env::var("BAAZ_DONE_CLOSE_SECS").ok().and_then(|v| v.parse::<u64>().ok()).map(|s| s * 1000).unwrap_or(DONE_CLOSES_AFTER_MS);
        let due = self.browser.update(cx, |browser, _| done_sessions_due(&sessions, now, after, &mut browser.task_seen_working, &mut browser.task_done_at));
        // Nothing reports a folder coming due: the view wakes the sweep
        // when the next one does.
        let next = self.browser.read(cx).task_done_at.values().map(|since| (since + after).saturating_sub(now)).min();
        if let Some(wait) = next {
            self.browser.update(cx, |browser, cx| browser.arm_done_tasks(wait, cx));
        }
        let mut closing = Vec::new();
        for session in due {
            let owner = crate::browser::Owner::Session(session.clone());
            let ids: Vec<_> = self.browser.read(cx).tabs.tabs_of(&owner).iter().map(|tab| tab.id).filter(|id| Some(*id) != shown).collect();
            if ids.is_empty() {
                continue;
            }
            crate::baaz_log!("done task folder closes {} tab(s) session={session}", ids.len());
            self.browser.update(cx, |browser, _| browser.task_done_at.remove(&session));
            self.browser.update(cx, |browser, _| browser.task_seen_working.remove(&session));
            closing.extend(ids);
        }
        // Closing builds the next tab's page, which needs the window: close
        // on the next turn, through the window this root draws in.
        if let (false, Some(handle)) = (closing.is_empty(), self.window_sync.handle) {
            cx.spawn(async move |this, cx| {
                let _ = handle.update(cx, |_, window, cx| {
                    let _ = this.update(cx, |this, cx| this.close_tabs(closing, window, cx));
                });
            })
            .detach();
        }
    }
}

impl Harness {
    /// A browser chat's first page of its own: a tab in its task folder with
    /// a live page the tools can drive (R3). Its later tabs come through
    /// `browser_tabs`. Nothing when it already has one.
    pub(crate) fn open_browser_chat_tab(&mut self, session: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        let owner = crate::browser::Owner::Session(session.to_owned());
        let tab = match self.browser.read(cx).tabs.tabs_of(&owner).first().map(|tab| tab.id) {
            Some(tab) => tab,
            None => {
                let space = self.browser.read(cx).browse_space();
                let tab = self.browser.update(cx, |bv, _| bv.tabs.open(space, owner, crate::browser::tabs::BLANK, None));
                self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
                tab
            }
        };
        self.ensure_tab_page(session, tab, window, cx);
        cx.notify();
    }
}

/// The "This page" chip.
pub(crate) fn page_chip() -> aui::composer::ComposerChip {
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

#[cfg(feature = "probe")]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_preamble_names_the_tab_and_strips_back_off() {
        let prompt = page_prompt("what's the refund window?", Some((7, "Returns", "https://shop.example/returns")));
        assert!(prompt.contains("tab=7"));
        assert_eq!(strip_page_prompt(&prompt), "what's the refund window?");
        assert_eq!(strip_page_prompt(&page_prompt("hi", None)), "hi");
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
