//! Find in page (task 4.5 split of `browser/mod.rs`): the find strip's one
//! input, kept per tab, and its steps.

use super::*;

impl Harness {

    /// Open the find strip on the shown tab (B2a-H5): ⌘F in Browse with a
    /// page shown, or the ⋯ menu's "Find in page". Focuses the input
    /// prefilled with the tab's kept query; a stored query re-searches.
    /// Opening releases the page keyboard first, so typing lands in the
    /// find field instead of the page. A crashed page or a blank tab
    /// opens nothing.
    pub(crate) fn open_browse_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if crate::layout::Layout::get(cx).mode != crate::layout::Mode::Browse {
            return;
        }
        self.ensure_tabs_loaded(cx);
        let Some(id) = self.browser.read(cx).browse_centre_shown(cx) else { return };
        let Some(state) = self.browse_centre_page(window, cx) else { return };
        if state.read(cx).crashed().is_some() {
            return;
        }
        if matches!(state.read(cx).url().as_ref(), "" | BLANK) {
            return;
        }
        state.update(cx, |state, _| state.release_keyboard());
        let (query, match_case, research) = self.browser.update(cx, |browser, _| {
            let strip = browser.find_strips.for_tab(id);
            let research = strip.open();
            (strip.query.clone(), strip.match_case, research)
        });
        self.focus_browse_find_input(&query, window, cx);
        if research {
            state.update(cx, |state, _| state.find(&query, true, match_case));
        }
        cx.notify();
    }

    /// Close the find strip (Done, Esc): `stop_find` clears the highlight.
    /// Closing a closed strip is a no-op, never a second `stop_find`. The
    /// shared input lives while any tab's strip stands open, so closing
    /// one tab's strip never strands the others'.
    pub(crate) fn close_browse_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.browser.read(cx).browse_centre_shown(cx) else { return };
        let stop = self.browser.update(cx, |browser, _| browser.find_strips.for_tab(id).close());
        if !self.browser.read(cx).find_strips.any_open() {
            self.browser.update(cx, |browser, _| browser.browse_find = None);
            self.browser.update(cx, |browser, _| browser.browse_find_sub = None);
        }
        if stop {
            if let Some(state) = self.browse_centre_state(cx) {
                state.update(cx, |state, _| state.stop_find());
            }
        }
        window.focus(&self.focus_root, cx);
        cx.notify();
    }

    /// Clear the tab's find highlight as it is left behind (a switch or a
    /// crash): the strip itself stays as it stood, so returning restores
    /// the query — only the page's highlight drops.
    /// Apply a pending centre switch to find (from `on_frame`): stop the
    /// left tab's highlight, point the shared input at the shown tab's
    /// query and search it again so its count and highlight come back.
    pub(crate) fn apply_find_resync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((left, shown)) = self.browser.update(cx, |browser, _| browser.find_resync.take()) else { return };
        if let Some(left) = left {
            self.browser.update(cx, |browser, cx| browser.stop_find_on_tab(left, cx));
        }
        if let Some(shown) = shown {
            self.sync_find_input_to_tab(shown, window, cx);
            if self.browser.read(cx).find_strips.visible(Some(shown)).is_some() {
                self.step_browse_find(true, cx);
            }
        }
    }

    /// Point the shared find input at the tab's kept query (a tab switch):
    /// the value is set here, on the event — never during render.
    pub(super) fn sync_find_input_to_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let (open, query) = match self.browser.read(cx).find_strips.visible(Some(id)) {
            Some((_, strip)) => (true, strip.query.clone()),
            None => return,
        };
        if open {
            if let Some(input) = self.browser.read(cx).browse_find.clone() {
                input.update(cx, |input, cx| {
                    input.set_value(query, window, cx);
                });
            }
        }
    }

    /// Flip "Match case" and re-search the kept query.
    pub(crate) fn toggle_browse_find_case(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.browser.read(cx).browse_centre_shown(cx) else { return };
        let research = self.browser.update(cx, |browser, _| browser.find_strips.for_tab(id).toggle_case());
        if research {
            self.step_browse_find(true, cx);
        }
        cx.notify();
    }

    /// Focus the find input with `text`, creating it on first use. The
    /// subscription types live (debounced) and commits Enter to the next
    /// match; Escape never reaches here (the wrapper owns it, like the
    /// address field), and blur drops nothing — the strip closes through
    /// Done, Esc or a tab switch, never by clicking the page.
    pub(super) fn focus_browse_find_input(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.to_owned();
        match self.browser.read(cx).browse_find.clone() {
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
                    input.set_placeholder("Find in page", window, cx);
                    input.set_value(text, window, cx);
                    input.select_all(window, cx);
                    input.focus(window, cx);
                });
                let sub = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::PressEnter { .. } => this.step_browse_find(true, cx),
                    InputEvent::Change => this.find_input_changed(cx),
                    InputEvent::Blur | InputEvent::Focus => {}
                });
                self.browser.update(cx, |browser, _| browser.browse_find = Some(input));
                self.browser.update(cx, |browser, _| browser.browse_find_sub = Some(sub));
            }
        }
        cx.notify();
    }

    /// Live search as the query types: a changed non-empty query searches
    /// 100 ms after the last keystroke; clearing the field stops nothing.
    /// The debounce is per tab: the timer fires only for the tab that
    /// armed it, so a switch never searches a stale query on the new tab.
    pub(super) fn find_input_changed(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.browser.read(cx).browse_centre_shown(cx) else { return };
        let typed = self.browser.read(cx).browse_find.as_ref().map(|input| input.read(cx).value().to_string());
        let Some(typed) = typed else { return };
        if !self.browser.update(cx, |browser, _| browser.find_strips.for_tab(id).set_query(&typed)) {
            return;
        }
        let generation = self.browser.update(cx, |browser, _| browser.find_strips.arm(id));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(chrome::FIND_DEBOUNCE_MS)).await;
            let _ = this.update(cx, |this, cx| {
                if this.browser.read(cx).find_strips.armed_current(id, generation)
                    && this.browser.read(cx).browse_centre_shown(cx) == Some(id)
                {
                    this.step_browse_find(true, cx);
                }
            });
        })
        .detach();
    }
}

impl BrowserView {

    pub(super) fn stop_find_on_tab(&mut self, id: TabId, cx: &mut gpui::App) {
        let open = self.find_strips.for_tab(id).open;
        if open {
            if let Some(state) = self.states.get(&id).cloned() {
                state.update(cx, |state, _| state.stop_find());
            }
        }
    }
}
