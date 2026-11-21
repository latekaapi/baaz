//! The Field — Browse's omnibox (task 4.5 split of `browser/mod.rs`): the
//! address input, its suggestions (tabs, sessions, history, bookmarks,
//! commands), the Ask row and the new tab's Pick up list.

use super::*;
use crate::layout::LayoutGlobal as _;

impl Harness {

    /// ⌘L from anywhere at window level: begin editing the address field
    /// (whole URL selected) when the right pane shows the Browser, and
    /// otherwise ignore the key. The webview's own `cmd-l` binding only
    /// fires while gpui holds the keyboard — with the native page focused
    /// the keystroke never reaches the pane, so this root binding carries
    /// it ([`aui_webview::FocusAddress`]). Already editing is a no-op, so
    /// a press that already reached the pane does not restart the edit.
    pub(crate) fn focus_browser_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // In Browse ⌘L reaches the centre toolbar's own field, which needs
        // no open pane: the Harness-owned input, prefilled with the shown
        // URL (empty on a blank tab, where the new-tab field takes it).
        if crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse {
            self.ensure_browse_tab(window, cx);
            let url = self
                .browser.read(cx).browse_centre_shown(cx)
                .and_then(|id| {
                    self.browser
                        .read(cx)
                        .states
                        .get(&id)
                        .map(|state| state.read(cx).url().to_string())
                        .or_else(|| self.browser.read(cx).tabs.get(id).map(|tab| tab.url.clone()))
                })
                .unwrap_or_default();
            let prefill = if url.is_empty() || url == BLANK { "" } else { url.as_str() };
            self.focus_browse_address_with(prefill, window, cx);
            return;
        }
        if !browser_pane_showing(
            crate::layout::Layout::get(cx).right_open,
            crate::layout::right_kind(crate::layout::Layout::get(cx)) == RightKind::Browser,
        ) {
            return;
        }
        // Q4c: while a provider switch is pending the pane is neutral — no
        // webview shows — so ⌘L reaches nothing.
        if self.pending_switch_dest(cx).is_some() {
            return;
        }
        let key = self.browser_key(cx);
        let state = self.browser_for(&key, false, window, cx);
        // Always (re)focus, even when the field still says it is editing: a
        // click into a Chromium page moves no gpui focus, so the field never
        // blurs and stays "editing" — returning early made every later ⌘L a
        // no-op (B1b, live). `begin_editing` reuses the open input.
        state.update(cx, |state, cx| state.begin_editing(window, cx));
    }

    /// Whether the Browse omnibox suggestion list stands open (B2a-H4):
    /// Browse shows the centre page and the field actually ranks rows —
    /// merely editing with no rows obscures nothing. The covering input
    /// in `Harness::page_covers`: the list drops down over the page.
    pub(crate) fn browse_omnibox_open(&self, cx: &gpui::App) -> bool {
        crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse && !self.browse_suggestions(cx).is_empty()
    }

    /// Focus the Browse address field with `text` selected (⌘L, a click on
    /// the resting field, ⌘T on a blank tab): the one Harness-owned input
    /// the toolbar and the new-tab field share — only one ever renders it
    /// per frame — so the omnibox reads one value everywhere. Reuses the
    /// open input when one exists. Records whether the page held the
    /// keyboard, so leaving the field can hand focus back to it.
    pub(crate) fn focus_browse_address_with(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.browser.update(cx, |browser, _| browser.browse_omnibox_selected = 0);
        self.browser.update(cx, |browser, _| browser.browse_omnibox_moved = false);
        self.browser.update(cx, |browser, _| browser.browse_completed = None);
        self.browser.update(cx, |browser, _| browser.browse_completed_text = None);
        // "Send selection to session…" is listed once the page says what is
        // selected (docs/82 8.3).
        if let Some(tab) = self.browser.read(cx).browse_centre_shown(cx) {
            self.browser.update(cx, |browser, cx| browser.ask_selection(tab, cx));
        }
        let page_focus = self.browse_centre_state(cx).is_some_and(|state| state.read(cx).holds_keyboard());
        self.browser.update(cx, |browser, _| browser.browse_address_page_focus = page_focus);
        // The page lets go of the keyboard as the field opens, as find does: a
        // page left key would take the keystrokes back the moment the Field's
        // panel closes.
        if let Some(state) = self.browse_centre_state(cx) {
            state.update(cx, |state, _| state.release_keyboard());
        }
        if std::env::var("BAAZ_OVERLAY_TRACE").as_deref() == Ok("1") {
            eprintln!("overlay: address focus asked; {}", crate::overlay::key_window_desc());
        }
        let text = text.to_owned();
        match self.browser.read(cx).browse_address.clone() {
            Some(input) => {
                input.update(cx, |input, cx| {
                    input.set_value(text, window, cx);
                    input.select_all(window, cx);
                    input.focus(window, cx);
                });
            }
            None => {
                let input = cx.new(|cx| InputState::new(window, cx));
                input.update(cx, |input, cx| {
                    input.set_placeholder(FIELD_PLACEHOLDER, window, cx);
                    input.set_value(text, window, cx);
                    input.select_all(window, cx);
                    input.focus(window, cx);
                });
                let sub = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
                    // ⌘↵ arrives here (not through the commit below): the
                    // input's own `secondary-enter` binding emits
                    // `PressEnter { secondary: true }` in the input context,
                    // so Ask never also fires the plain-enter commit.
                    InputEvent::PressEnter { secondary: true, .. } => this.ask_from_browse_address(window, cx),
                    InputEvent::PressEnter { .. } => this.commit_browse_address(window, cx),
                    // Blur only ends editing — never moves focus (the user
                    // clicked something focusable). Focus restore belongs
                    // only to Escape / Enter / row activation.
                    // A blur while the window is not key is the keyboard
                    // leaving for one of our own page windows (a new tab's
                    // hidden Chromium window is key for a moment when it is
                    // created — found live) or another app: the edit stays.
                    // A real click into a visible page ends it through the
                    // page-took-the-keyboard rule in `BrowserView::occlude`.
                    InputEvent::Blur => {
                        if window.is_window_active() {
                            this.browser.update(cx, |browser, cx| browser.drop_browse_address(cx));
                        }
                    }
                    InputEvent::Focus => this.browser.update(cx, |browser, cx| browser.move_omnibox(OmniboxStep::Reset { forget_completion: true }, cx)),
                    InputEvent::Change => {
                        // Typing resets the highlight. A Tab completion
                        // survives only its own write: once the text differs
                        // from what Tab wrote, the person is typing something
                        // else and Enter must open that, not the old row.
                        let typed = this.browser.read(cx).browse_address.as_ref().map(|input| input.read(cx).value().to_string());
                        let stale = this.browser.read(cx).browse_completed_text.is_some() && this.browser.read(cx).browse_completed_text != typed;
                        this.browser.update(cx, |browser, cx| browser.move_omnibox(OmniboxStep::Reset { forget_completion: stale }, cx));
                    }
                });
                self.browser.update(cx, |browser, _| browser.browse_address = Some(input));
                self.browser.update(cx, |browser, _| browser.browse_address_sub = Some(sub));
            }
        }
        cx.notify();
    }


    /// Leave the address field without navigating: the resting view falls
    /// back to the page URL on the next frame. Runs on Escape, on Enter
    /// (commit/Ask paths) and on row activation — never on blur, which
    /// only ends editing without moving focus. While still
    /// in Browse, focus returns to the page when it held the keyboard
    /// before the edit, else to the root — never a destroyed handle (the
    /// page counts only while its state still exists).
    pub(crate) fn leave_browse_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.browser.read(cx).browse_address.is_none() {
            return;
        }
        self.browser.update(cx, |browser, cx| browser.drop_browse_address(cx));
        if crate::layout::Layout::get(cx).mode != crate::layout::Mode::Browse {
            return;
        }
        if self.browser.read(cx).browse_address_page_focus {
            if let Some(state) = self.browse_centre_state(cx) {
                let focus = state.read(cx).focus_handle().clone();
                window.focus(&focus, cx);
                return;
            }
        }
        window.focus(&self.focus_root, cx);
    }

    /// Commit the address field (Enter): the highlighted omnibox row opens
    /// once the highlight moved off the top Ask row — otherwise the typed
    /// text navigates (a URL, or what looks like a host) or searches with
    /// the default engine — and empty restores the field, never navigating.
    /// Runs on the input's enter.
    pub(crate) fn commit_browse_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A Tab completion remembers its ROW: Enter opens that row even
        // after the ranking moved under it — never an index into the
        // re-ranked list.
        if let Some(target) = self.browser.read(cx).browse_completed.clone() {
            return self.activate_omnibox_target(target, window, cx);
        }
        // In the Bookmarks scope ↩ always opens the highlighted row (a
        // folder or a bookmark): the typed text only filters there.
        if self.browser.read(cx).browse_omnibox_moved || self.browser.read(cx).field_scope.is_some() {
            let rows = self.browse_suggestions(cx);
            if rows.is_empty() {
                return;
            }
            let index = self.browser.read(cx).browse_omnibox_selected.min(rows.len().saturating_sub(1));
            return self.activate_omnibox_row(index, window, cx);
        }
        let typed = self.browser.read(cx).browse_address_text(cx);
        self.leave_browse_address(window, cx);
        self.ensure_tabs_loaded(cx);
        let Some(id) = self.browser.read(cx).browse_centre_shown(cx) else {
            return;
        };
        match toolbar::classify_address_input_with(&typed, crate::layout::Layout::get(cx).search_engine) {
            toolbar::NavTarget::Url(url) => self.navigate_browse_tab(id, &url, window, cx),
            toolbar::NavTarget::Search(url) => {
                if !url.is_empty() {
                    self.navigate_browse_tab(id, &url, window, cx);
                } else {
                    cx.notify();
                }
            }
        }
    }

    /// The Field's Ask (its Ask row, ⌘↵): the one Ask (docs/82 8.2) —
    /// the ⌘J line opens on the page shown and asks `query` there in
    /// place, as the line's own submit would. Never a session, never a
    /// draft; "Continue in a session…" is the card's second step. With no
    /// page shown (a new tab) it still asks on the line, with no page.
    pub(crate) fn browse_ask(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        let query = query.trim().to_owned();
        if query.is_empty() {
            return;
        }
        self.leave_browse_address(window, cx);
        self.open_agent_line(window, cx);
        self.ask_browser_chat(&query, true, false, window, cx);
        cx.notify();
    }

    /// ⌘↵ inside the address field: Ask about the field's text. Runs from
    /// the input's own `secondary-enter` binding (see
    /// [`Self::focus_browse_address_with`]), so the plain-enter commit
    /// never also fires.
    pub(crate) fn ask_from_browse_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.browser.read(cx).browse_address_text(cx);
        self.browse_ask(&query, window, cx);
    }

    /// The omnibox rows for the field's current text: this project's open
    /// tabs (person tabs plus session tabs), its sessions, commands and
    /// history from `browser.db` (R4). Sessions
    /// stay inside this project and out of the hidden list.
    pub(crate) fn browse_suggestions(&self, cx: &gpui::App) -> Vec<OmniRow> {
        let query = self.browser.read(cx).browse_address_text(cx);
        if let Some(folder) = self.browser.read(cx).field_scope {
            return self.browser.read(cx).data.read(cx).bookmark_scope_rows(&query, folder);
        }
        if query.trim().is_empty() || !self.browser.read(cx).tabs_loaded {
            return Vec::new();
        }
        let space = self.browser.read(cx).browse_space();
        let mut tabs: Vec<(TabId, String, String, u64)> = self
            .browser
            .read(cx)
            .tabs
            .person_tabs(space.as_deref())
            .iter()
            .map(|tab| {
                let label = tabs::tab_label(&tab.title, &tab.url);
                (
                    tab.id,
                    if label.is_empty() { tab.url.clone() } else { label },
                    tab.url.clone(),
                    tab.last_shown.unwrap_or(0),
                )
            })
            .collect();
        for (_, group) in self.browser.read(cx).tabs.session_tabs(space.as_deref()) {
            for tab in group {
                let label = tabs::tab_label(&tab.title, &tab.url);
                tabs.push((
                    tab.id,
                    if label.is_empty() { tab.url.clone() } else { label },
                    tab.url.clone(),
                    tab.last_shown.unwrap_or(0),
                ));
            }
        }
        let current = self.current_project_id();
        let sessions: Vec<(String, String, u64)> = self
            .session_list.read(cx).sessions
            .iter()
            .filter(|entry| !entry.archived && !entry.hidden && entry.project == current)
            .map(|entry| (entry.id.clone(), entry.label.clone(), recency_ms(&entry.updated)))
            .collect();
        let offers = self.shown_page_offers(cx);
        let commands: Vec<(String, String)> = FIELD_COMMANDS
            .iter()
            .filter(|(_, _, action)| field_command_offered(*action, offers))
            .map(|(id, title, _)| ((*id).to_owned(), (*title).to_owned()))
            .collect();
        let data = self.browser.read(cx).data.read(cx);
        let history = data.history_hits(&query);
        let marks = data.bookmark_hits(&query);
        toolbar::rank_omnibox(&query, &tabs, &sessions, &commands, &history, &marks, crate::layout::Layout::get(cx).search_engine)
    }

    /// ⌫ on an empty query in the scoped view: up one folder, or out of
    /// the scope at the top. Returns whether it did anything.
    pub(crate) fn field_scope_up(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(folder) = self.browser.read(cx).field_scope else { return false };
        let scope = match folder {
            Some(id) => {
                Some(self.browser.read(cx).data.read(cx).folder_parent(id))
            }
            None => None,
        };
        self.browser.update(cx, |browser, cx| browser.move_omnibox(OmniboxStep::Scope(scope), cx));
        true
    }


    /// Move the omnibox highlight by `delta`, wrapping over the rows the
    /// field currently ranks. An explicit move supersedes any Tab
    /// completion: the highlight, not the completed row, now owns Enter.
    pub(crate) fn move_omnibox(&mut self, delta: isize, cx: &mut Context<Self>) {
        let rows = self.browse_suggestions(cx);
        if rows.is_empty() {
            return;
        }
        let len = rows.len() as isize;
        let next = (self.browser.read(cx).browse_omnibox_selected as isize + delta).rem_euclid(len);
        self.browser.update(cx, |browser, cx| browser.move_omnibox(OmniboxStep::Arrow { index: next as usize, forget_completion: true }, cx));
    }

    /// Complete the highlighted row's text into the field (Tab): tabs write
    /// their URL, sessions their title; Ask and Search rows are not
    /// completable and Tab leaves them alone. The selection is clamped to
    /// the rows and kept, and the completed ROW's identity is remembered —
    /// never an index into the re-ranked list — so Enter opens that row.
    /// The row stays open for Enter or ⌘↵.
    pub(crate) fn complete_omnibox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.browse_suggestions(cx);
        if rows.is_empty() {
            return;
        }
        let index = self.browser.read(cx).browse_omnibox_selected.min(rows.len() - 1);
        let row = rows[index].clone();
        let Some(text) = row.suggestion.complete.clone() else {
            return;
        };
        if let Some(input) = self.browser.read(cx).browse_address.clone() {
            self.browser.update(cx, |browser, _| browser.browse_completed_text = Some(text.clone()));
            input.update(cx, |input, cx| {
                input.set_value(text, window, cx);
                input.select_all(window, cx);
                input.focus(window, cx);
            });
        }
        // The input's own Change arm resets the highlight (keeping the
        // completion, which survives the re-rank); the completed row keeps
        // the highlight too, and its identity owns Enter.
        self.browser.update(cx, |browser, cx| browser.move_omnibox(OmniboxStep::Complete { index, row: row.target.clone() }, cx));
    }

    /// Activate an omnibox row (Enter opens): Ask asks, a person tab
    /// selects in Browse (clearing any carried tab), a session tab opens
    /// its session in Sessions (updating the sidebar selection there), a
    /// session opens in Sessions, and history/search URLs load in the
    /// shown tab. The index clamps to the rows, so a stale highlight can
    /// never miss into nothing.
    pub(crate) fn activate_omnibox_row(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.browse_suggestions(cx);
        let index = index.min(rows.len().saturating_sub(1));
        let Some(row) = rows.get(index).cloned() else {
            return;
        };
        self.activate_omnibox_target(row.target, window, cx);
    }

    /// Activate an omnibox row by identity (a Tab completion's remembered
    /// row): the same destinations as [`Self::activate_omnibox_row`],
    /// without re-resolving an index into a re-ranked list.
    pub(crate) fn activate_omnibox_target(
        &mut self,
        target: OmniTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            OmniTarget::Ask(query) => self.browse_ask(&query, window, cx),
            OmniTarget::Command(id) => {
                self.leave_browse_address(window, cx);
                match FIELD_COMMANDS.iter().find(|(command, _, _)| *command == id).map(|(_, _, action)| *action) {
                    Some(FieldAction::Page(action)) => self.browse_page_action(action, window, cx),
                    Some(FieldAction::ForgetSite) => self.forget_shown_site(cx),
                    Some(FieldAction::ClearHistory) => self.ask_clear_history(cx),
                    Some(FieldAction::Bookmark) => self.bookmark_shown_page(window, cx),
                    Some(FieldAction::AdsHere) => self.toggle_ads_here(window, cx),
                    Some(FieldAction::Bookmarks) => {
                        // Back into the Field, scoped (the leave above closed it).
                        self.browser.update(cx, |browser, _| browser.field_scope = Some(None));
                        self.focus_browse_address_with("", window, cx);
                        self.browser.update(cx, |browser, _| browser.field_scope = Some(None));
                    }
                    None => {}
                }
            }
            OmniTarget::Folder(id) => {
                self.browser.update(cx, |browser, cx| browser.move_omnibox(OmniboxStep::Scope(Some(Some(id))), cx));
                if let Some(input) = self.browser.read(cx).browse_address.clone() {
                    input.update(cx, |input, cx| input.set_value("", window, cx));
                }
            }
            OmniTarget::Tab(id) => {
                let owner = self.browser.read(cx).tabs.get(id).map(|tab| tab.owner.clone());
                match owner {
                    Some(Owner::Person) => self.select_browse_sidebar_tab(id, window, cx),
                    Some(Owner::Session(session)) => {
                        self.leave_browse_address(window, cx);
                        // A mouse-down already opened it: the click that
                        // follows is a no-op, never a second open.
                        if self.active_id(cx).as_deref() != Some(session.as_str()) {
                            self.set_mode(crate::layout::Mode::Sessions, window, cx);
                            self.resume(session, window, cx);
                        }
                    }
                    None => {}
                }
            }
            OmniTarget::Session(session) => {
                self.leave_browse_address(window, cx);
                // A mouse-down already opened it: the click that follows
                // is a no-op, never a second open.
                if self.active_id(cx).as_deref() != Some(session.as_str()) {
                    self.set_mode(crate::layout::Mode::Sessions, window, cx);
                    self.resume(session, window, cx);
                }
            }
            OmniTarget::History(url) | OmniTarget::Search(url) => {
                self.leave_browse_address(window, cx);
                self.ensure_tabs_loaded(cx);
                if let Some(id) = self.browser.read(cx).browse_centre_shown(cx) {
                    self.navigate_browse_tab(id, &url, window, cx);
                }
            }
        }
    }

    /// The new-tab page's pick-up rows (B2a-H4) with what each opens: this
    /// project's open tabs (blank ones excluded — the current empty tab is
    /// not somewhere to pick up), then its recent sessions, then recently
    /// closed tabs — capped at 5.
    pub(crate) fn browse_pickup_full(&self, cx: &gpui::App) -> Vec<(toolbar::PickupRow, PickupTarget)> {
        let space = self.browser.read(cx).browse_space();
        let project_name = self.current_project(cx).map(|project| project.name.clone()).unwrap_or_default();
        let tab_items: Vec<(TabId, String, String)> = if self.browser.read(cx).tabs_loaded {
            self.browser
                .read(cx)
                .tabs
                .person_tabs(space.as_deref())
                .iter()
                .filter(|tab| !tab.url.is_empty() && tab.url != BLANK)
                .map(|tab| {
                    let label = tabs::tab_label(&tab.title, &tab.url);
                    (tab.id, if label.is_empty() { tab.url.clone() } else { label }, project_name.clone())
                })
                .collect()
        } else {
            Vec::new()
        };
        let current = self.current_project_id();
        let session_items: Vec<(String, String, String)> = self
            .session_list.read(cx).sessions
            .iter()
            .filter(|entry| !entry.archived && !entry.hidden && entry.project == current)
            .take(5)
            .map(|entry| (entry.id.clone(), entry.label.clone(), project_name.clone()))
            .collect();
        // Recently closed tabs are this project's person tabs only — another
        // project's tabs never offer here — newest first, and clickable:
        // each row reopens exactly the tab it names.
        let now = tabs::now_ms();
        let closed_items: Vec<(TabId, String, String)> = self
            .browser
            .read(cx)
            .tabs
            .closed
            .iter()
            .rev()
            // Blank tabs recorded before 2026-10-13 are skipped the same way
            // new ones are never recorded.
            .filter(|tab| tab.owner == Owner::Person && tab.space == space && !tabs::is_blank_url(&tab.url))
            .take(5)
            .map(|tab| {
                let ago = tab
                    .last_shown
                    .map(|shown| toolbar::relative_ago(now.saturating_sub(shown)))
                    .unwrap_or_else(|| "a while ago".to_owned());
                let label = tabs::tab_label(&tab.title, &tab.url);
                (tab.id, if label.is_empty() { tab.url.clone() } else { label }, ago)
            })
            .collect();
        let tab_pairs: Vec<(String, String)> =
            tab_items.iter().map(|(_, title, project)| (title.clone(), project.clone())).collect();
        let session_pairs: Vec<(String, String)> =
            session_items.iter().map(|(_, label, project)| (label.clone(), project.clone())).collect();
        let closed_pairs: Vec<(String, String)> =
            closed_items.iter().map(|(_, title, ago)| (title.clone(), ago.clone())).collect();
        let display = toolbar::pickup_rows(&tab_pairs, &session_pairs, &closed_pairs, 5);
        let mut targets = Vec::new();
        for (id, _, _) in &tab_items {
            targets.push(PickupTarget::Tab(*id));
        }
        for (id, _, _) in &session_items {
            targets.push(PickupTarget::Session(id.clone()));
        }
        for (id, _, _) in &closed_items {
            targets.push(PickupTarget::Closed(*id));
        }
        targets.truncate(5);
        display.into_iter().zip(targets).collect()
    }
}

/// One move of the omnibox ([`BrowserView::move_omnibox`]).
pub(crate) enum OmniboxStep {
    /// Back on the first row; `forget_completion` drops a Tab completion
    /// (its row and the text it wrote) too.
    Reset { forget_completion: bool },
    /// The arrows' highlight on row `index`; a completion it supersedes
    /// goes when `forget_completion`.
    Arrow { index: usize, forget_completion: bool },
    /// The pointer over row `index`.
    Hover(usize),
    /// A Tab completion of `row` at `index`.
    Complete { index: usize, row: crate::browser::OmniTarget },
    /// The folder scope.
    Scope(Option<Option<i64>>),
}

impl BrowserView {
    /// Move the omnibox's highlight (or its scope) — the field's one owner
    /// method — and redraw the rows when anything moved (or the text did).
    pub(crate) fn move_omnibox(&mut self, step: OmniboxStep, cx: &mut Context<Self>) {
        let before = (self.browse_omnibox_selected, self.browse_omnibox_moved, self.browse_completed.clone(), self.browse_completed_text.clone(), self.field_scope);
        // A reset always redraws: it follows typing (and a fresh focus), and
        // the rows are the typed text's.
        let typed = matches!(step, OmniboxStep::Reset { .. });
        let forget = match step {
            // Back on the first row, as a fresh focus or typing leaves it.
            OmniboxStep::Reset { forget_completion } => {
                self.browse_omnibox_selected = 0;
                self.browse_omnibox_moved = false;
                forget_completion
            }
            // The arrows put the highlight on a row: it owns Enter now
            // (`commit_browse_address`).
            OmniboxStep::Arrow { index, forget_completion } => {
                self.browse_omnibox_selected = index;
                self.browse_omnibox_moved = true;
                forget_completion
            }
            // The pointer over a row lights it (no ownership of Enter).
            OmniboxStep::Hover(index) => {
                self.browse_omnibox_selected = index;
                false
            }
            // A Tab completion of a row: its identity owns Enter.
            OmniboxStep::Complete { index, row } => {
                self.browse_omnibox_selected = index;
                self.browse_omnibox_moved = true;
                self.browse_completed = Some(row);
                false
            }
            // Scoped to a bookmarks folder (`Some(None)` the top,
            // `Some(Some(id))` a folder) or unscoped, back on top.
            OmniboxStep::Scope(scope) => {
                self.field_scope = scope;
                self.browse_omnibox_selected = 0;
                false
            }
        };
        if forget {
            self.browse_completed = None;
            self.browse_completed_text = None;
        }
        let after = (self.browse_omnibox_selected, self.browse_omnibox_moved, self.browse_completed.clone(), self.browse_completed_text.clone(), self.field_scope);
        if typed || before != after {
            cx.notify();
        }
    }

    /// The sidebar's one name field: on a folder or a space (`Some`), or
    /// gone. Redraws when it moved; whether it did.
    pub(crate) fn set_naming(&mut self, naming: Option<aui::screens::sidebar::BrowseNaming>, cx: &mut Context<Self>) -> bool {
        if self.browse_naming == naming {
            return false;
        }
        self.browse_naming = naming;
        cx.notify();
        true
    }

    /// A tab drag ended (dropped or left): no row stands lit as its target.
    pub(crate) fn clear_drop(&mut self, cx: &mut Context<Self>) {
        if self.browse_drop_target.is_some() || self.browse_drop_at_end {
            self.browse_drop_target = None;
            self.browse_drop_at_end = false;
            cx.notify();
        }
    }

    /// The tab the carried session shows in Browse (`None`: the person's
    /// own tab). Redraws when it moved; whether it did.
    pub(crate) fn set_session_tab(&mut self, tab: Option<TabId>, cx: &mut Context<Self>) -> bool {
        if self.browse_session_tab == tab {
            return false;
        }
        self.browse_session_tab = tab;
        cx.notify();
        true
    }

    /// Drop the address input without navigating: the resting view falls
    /// back to the page URL on the next frame. The window-free core of
    /// [`Self::leave_browse_address`], for the project-adoption paths that
    /// own no window.
    pub(crate) fn drop_browse_address(&mut self, cx: &mut Context<Self>) {
        self.field_scope = None;
        self.placeholder_scoped = false;
        if self.browse_address.is_some() {
            self.browse_address = None;
            self.browse_address_sub = None;
            self.browse_omnibox_selected = 0;
            self.browse_omnibox_moved = false;
            self.browse_completed = None;
            self.browse_completed_text = None;
            cx.notify();
        }
    }



    /// The address field's current text, or empty at rest.
    pub(crate) fn browse_address_text(&self, cx: &gpui::App) -> String {
        self.browse_address
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default()
    }
}
