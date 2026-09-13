//! One frame of the centre pane: the transcript, the status row, the
//! banner and the composer.
//!
//! Part of [`SessionView`]; see [`crate::session`] for what
//! the entity owns and why these are its own files.

use super::*;
use std::path::Path;
use aui_tokens::ActiveAui;

impl SessionView {
    // ----------------------------------------------------------------- render

    /// The centre pane: transcript, status row, banner, composer.
    pub fn render_centre(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if let Some(text) = self.pending_prompt.take() {
            self.composer.update(cx, |state, cx| state.set_value(text, window, cx));
            self.note_draft(cx);
        }
        let transcript = self.render_transcript(window, cx);
        // A `--screenshot` run that asked for an approval waits for one; this
        // is where the capture learns that it arrived (finding F9). After the
        // transcript, because that is what refreshes the render cache the
        // pending-approval answer is now read from (finding `performance-2`).
        self.capture.set_pending_approval(self.newest_pending_approval().is_some());
        let status = self.render_status();
        let needs_you = self.render_needs_you(cx);
        let banner = self.render_banner(cx);
        let tier_banner = self.render_tier_banner(cx);
        let queue = self.render_queue(cx);
        let caret_menu = self.render_caret_menu(cx);
        let composer = self.render_composer(cx);
        let drop = self.dragging;
        let p = cx.aui().colors;
        v_flex()
            .size_full()
            .relative()
            .child(transcript)
            .children(status)
            .children(needs_you)
            .children(banner)
            .children(tier_banner)
            .children(queue)
            // The caret menus anchor to the measure: the positioned ancestor is
            // the centred inner wrapper, so the popover spans 880 px, not the
            // pane.
            .child(
                div().w_full().px(px(TRANSCRIPT_PAD_X)).child(
                    div()
                        .w_full()
                        .max_w(px(TRANSCRIPT_MEASURE))
                        .mx_auto()
                        .relative()
                        .children(caret_menu),
                ),
            )
            // The docked band spans the pane, hairline included; only the
            // composer's content is bound to the measure, as in the design.
            // The library's docked composer draws its own top hairline, which
            // would stop at the measure's edges: the band draws the pane-wide
            // one and the composer is pulled up a pixel so its own lies on it.
            .child(
                div().w_full().bg(p.surface_1).border_t_1().border_color(p.line).child(
                    div().w_full().max_w(px(TRANSCRIPT_MEASURE)).mx_auto().mt(px(-1.0)).child(composer),
                ),
            )
            .child(aui::composer::drop_overlay("drop", drop))
            // gpui reports an external drag only while it moves, so that is
            // what raises the overlay; the drop takes it down again.
            .on_drag_move(cx.listener(|this: &mut Self, _: &gpui::DragMoveEvent<ExternalPaths>, _, cx| {
                if !this.dragging {
                    this.dragging = true;
                    cx.notify();
                }
            }))
            .on_drop(cx.listener(|this: &mut Self, paths: &ExternalPaths, _, cx| {
                this.dragging = false;
                this.attach_paths(paths.paths().to_vec(), cx);
            }))
            .into_any_element()
    }

    /// One frame of the transcript: sync the cache, then compose from it.
    ///
    /// The split is the whole point (finding `app-core-11`). Everything that
    /// writes happens in [`Self::sync_render_cache`] and
    /// [`Self::sync_virtual_list`]; everything below them reads
    /// `cached_turns` / `cached_full_output` and never the live fold, so one
    /// frame draws one consistent snapshot rather than whatever the wire
    /// happened to have folded by the time a given row was built.
    pub(super) fn render_transcript(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        crate::log::trace_first_frame();
        let frame_start = std::time::Instant::now();
        self.sync_render_cache();
        if self.cached_turns.is_empty() {
            return self.empty_or_loading(window, cx);
        }
        let folds = self.fold_intents(window, cx);
        let element = self.transcript_list(folds, cx);
        record_frame_stats(frame_start.elapsed());
        let _ = window;
        element
    }

    /// The one place a frame writes the render cache.
    ///
    /// Steady-state frames share one snapshot: re-snapshot only when the fold
    /// grew or shrank under us, or when `follow` says the content changed.
    pub(super) fn sync_render_cache(&mut self) {
        let live_len = self.fold.session(&self.session_id).map(|s| s.turns.len()).unwrap_or(0);
        if live_len != self.cached_turns.len() || self.follow {
            self.refresh_render_cache();
        }
    }

    /// Nothing to show yet, and the two reasons are different.
    ///
    /// Loading is not empty: while the first backfill page is still on the
    /// wire this neutral row stands in — never `empty_state`. The switch
    /// itself is immediate, so this is what a fresh session shows on its
    /// first frames.
    pub(super) fn empty_or_loading(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.loading_history {
            return Self::loading_row();
        }
        // A replayed capture is read-only, so its empty state offers
        // nothing to type: the chips would be three buttons that refuse.
        let pick = (!self.replay).then(|| {
            let pick = cx.listener(|this: &mut Self, index: &usize, window, cx| {
                if let Some(text) = transcript::suggestion(*index) {
                    this.set_draft(text.to_owned(), window, cx);
                    this.focus_composer(window, cx);
                }
            });
            std::rc::Rc::new(move |index: usize, window: &mut Window, cx: &mut gpui::App| {
                pick(&index, window, cx)
            }) as transcript::PickSuggestion
        });
        let _ = window;
        // The project's display name, not the folder's: a rename changes one
        // without touching the other.
        let display = self.project_name.clone().unwrap_or_else(|| {
            std::path::Path::new(&self.workspace)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.workspace.clone())
        });
        transcript::empty_state(&display, pick, cx)
    }

    /// Every intent a card can raise, bound once per frame.
    pub(super) fn fold_intents(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Folds {
        Folds {
            toggled: Rc::clone(&self.toggled),
            toggle: {
                let toggle = cx.listener(|this: &mut Self, key: &String, _, cx| {
                    this.toggle_fold(key.clone(), cx);
                });
                Rc::new(move |key: String, window: &mut Window, cx: &mut gpui::App| toggle(&key, window, cx))
            },
            plan: {
                let act = cx.listener(|this: &mut Self, (id, action): &(String, PlanAction), window, cx| {
                    this.plan_action(id, *action, window, cx);
                });
                Some(Rc::new(move |id: String, action: PlanAction, window: &mut Window, cx: &mut gpui::App| {
                    act(&(id, action), window, cx)
                }))
            },
            // A replayed capture gets the same wiring: opening a preview and
            // picking an option are local, and anything that would reach the
            // wire is refused by `wire_client` with a banner that says why.
            cards: Some(self.card_intents(window, cx)),
            titles: Rc::clone(&self.titles),
            at_rest: self.at_rest,
            full_output: Rc::clone(&self.cached_full_output),
            show_full_output: {
                let show = cx.listener(|this: &mut Self, id: &String, _, cx| {
                    this.show_full_output(id.clone(), cx);
                });
                Some(Rc::new(move |id: String, window: &mut Window, cx: &mut gpui::App| {
                    show(&id, window, cx)
                }))
            },
            // Turn links and bottom-row actions (C5, C6): markdown URLs open
            // in the browser, workspace paths reveal in Finder, and every
            // wire action is live-only — replay answers with a toast.
            link: {
                let link = cx.listener(|this: &mut Self, target: &LinkTarget, _, cx| {
                    this.handle_link(target.clone(), cx);
                });
                Some(Rc::new(move |target: LinkTarget, window: &mut Window, cx: &mut gpui::App| {
                    link(&target, window, cx)
                }))
            },
            assistant_action: {
                let act = cx.listener(
                    |this: &mut Self, (id, action): &(String, AssistantTurnAction), window, cx| {
                        this.assistant_action(id.clone(), *action, window, cx);
                    },
                );
                Some(Rc::new(
                    move |id: String, action: AssistantTurnAction, window: &mut Window, cx: &mut gpui::App| {
                        act(&(id, action), window, cx)
                    },
                ))
            },
            user_action: {
                let act = cx.listener(
                    |this: &mut Self, (id, text, action): &(String, String, UserTurnAction), window, cx| {
                        this.user_action(id.clone(), text.clone(), *action, window, cx);
                    },
                );
                Some(Rc::new(
                    move |id: String,
                          text: String,
                          action: UserTurnAction,
                          window: &mut Window,
                          cx: &mut gpui::App| { act(&(id, text, action), window, cx) },
                ))
            },
            // Text selection (C8b): every turn gets its own held cell, and
            // every intent carries its turn's markdown source back.
            text_selections: Rc::clone(&self.text_selections),
            selection_change: {
                let changed = cx.listener(
                    |this: &mut Self,
                     (turn_id, source, next): &(String, String, Option<TextSelection>),
                     _,
                     cx| {
                        this.set_text_selection(turn_id.clone(), source.clone(), next.clone(), cx);
                    },
                );
                Some(Rc::new(
                    move |turn_id: String,
                          source: String,
                          next: Option<TextSelection>,
                          window: &mut Window,
                          cx: &mut gpui::App| { changed(&(turn_id, source, next), window, cx) },
                ))
            },
            tool_group: {
                let act = cx.listener(
                    |this: &mut Self, (key, intent): &(String, ToolGroupIntent), window, cx| {
                        this.tool_group_action(key.clone(), *intent, window, cx);
                    },
                );
                Some(Rc::new(
                    move |key: String, intent: ToolGroupIntent, window: &mut Window, cx: &mut gpui::App| {
                        act(&(key, intent), window, cx)
                    },
                ))
            },
        }
    }

    /// Bring the virtual list in step with the cache.
    ///
    /// One item per row (a block, a bubble, a silent footer), never per
    /// turn: gpui lays a visible item out whole every frame, and a real turn
    /// can run to hundreds of blocks, so per-turn items cost a frame what
    /// the biggest visible turn cost — 8 ms p90 on a real session, past a
    /// 120 Hz budget (owner round 2026-09-13, item 2).
    ///
    /// Heights: an unmeasured row with no hint counts as 0 px in the list's
    /// sum tree, so an upward flick over such rows clamps at the head and
    /// teleports there (H2). Every row therefore carries a hint until it is
    /// measured — on the first fill, after every history page (the
    /// 2026-09-12 fix hinted the first fill only), and again after gpui
    /// forgot every height on a width change. The splice is the changed
    /// range only: the rows above the first turn whose row count changed
    /// keep their measurements. A pure append (a streaming block) measures
    /// the new tail and nothing else; a page landing during the backfill
    /// re-hints instead, because a thousand events of unhinted rows above a
    /// tail-pinned reader is exactly the H2 stack.
    pub(super) fn sync_virtual_list(&mut self, counts: &[(String, usize)]) {
        let count: usize = counts.iter().map(|(_, n)| n).sum();
        let changed = count != self.list_len || counts != self.synced_counts.as_slice();
        if changed {
            let old = self.list_len;
            // The first row of the first turn whose `(id, rows)` differs from
            // what the list was last synced to; everything before it is
            // unchanged and stays measured.
            let same = counts
                .iter()
                .zip(self.synced_counts.iter())
                .take_while(|(a, b)| a == b)
                .count();
            let from: usize = counts.iter().take(same).map(|(_, n)| n).sum();
            self.list_len = count;
            self.synced_counts = counts.to_vec();
            if old == 0 || self.loading_history {
                self.rehint_rows(count);
            } else {
                self.list_state.splice(from..old, count - from);
            }
        } else if std::mem::take(&mut self.rehint) {
            self.rehint_rows(count);
        }
        if std::mem::take(&mut self.follow) && self.list_state.is_scrolled_to_end().unwrap_or(true) {
            self.list_state.scroll_to_end();
        }
    }

    /// Give every unmeasured row a hint, keeping measured heights (they
    /// become their own hints) and the scroll position. `reset` drops the
    /// scroll position and the wheel events until the next paint, so the
    /// position is put back by hand; one frame of dropped wheel events is the
    /// price, paid only on a page landing or a width change.
    fn rehint_rows(&mut self, count: usize) {
        let at_end = self.list_state.is_scrolled_to_end().unwrap_or(true);
        let top = self.list_state.logical_scroll_top();
        self.list_state.reset_with_uniform_height(count, px(ROW_HEIGHT_HINT));
        if at_end {
            self.list_state.scroll_to_end();
        } else if top.item_ix < count {
            self.list_state.scroll_to(top);
        }
    }

    /// The list's laid-out width, reported once per frame from the wrapper's
    /// prepaint. A change means gpui has just dropped every row height and
    /// hint; the next sync re-hints.
    pub(super) fn note_list_width(&mut self, width: f32) {
        if self.list_width.is_some_and(|last| (last - width).abs() > 0.5) {
            self.rehint = true;
        }
        self.list_width = Some(width);
    }

    /// The virtualised list itself, built from the cache and nothing else.
    pub(super) fn transcript_list(&mut self, folds: Folds, cx: &mut Context<Self>) -> AnyElement {
        let counts = std::mem::take(&mut self.row_counts);
        self.sync_virtual_list(&counts);
        self.row_counts = counts;
        let last_turn = self.cached_turns.len().saturating_sub(1);
        let last_row = self.rows.len().saturating_sub(1);
        // `Rc` clones: O(1). Only visible rows are built below.
        let turns = self.cached_turns.clone();
        let rows = self.rows.clone();
        let folds = Rc::new(folds);
        let width_report = cx.entity().downgrade();
        // The wrapper is a flex column so the virtual list's own
        //  resolves to the leftover centre height; without it the
        // list lays out at zero height and paints nothing. It carries
        // the pre-virtualised transcript's own gutters (pt/px/pb) because
        // the list items themselves are full-bleed rows; the px step is
        // the same TRANSCRIPT_PAD_X the status and banner rows use, so
        // the turns line up with them. The list itself sits in the centred
        // measure wrapper, so turns are capped at TRANSCRIPT_MEASURE.
        div()
            .w_full()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .flex_col()
            .px(px(TRANSCRIPT_PAD_X))
            .pb(px(scale::SP_4))
            .key_context(TRANSCRIPT_CONTEXT)
            .child(
                div()
                    .w_full()
                    .flex_1()
                    .min_h(px(0.0))
                    .flex()
                    .flex_col()
                    .max_w(px(TRANSCRIPT_MEASURE))
                    .mx_auto()
                    // The list's width, for the re-hint after a change.
                    .on_children_prepainted(move |bounds, _, cx| {
                        if let Some(first) = bounds.first() {
                            let width = f32::from(first.size.width);
                            let _ = width_report.update(cx, |view, _| view.note_list_width(width));
                        }
                    })
                    .child(
                        list(self.list_state.clone(), move |ix, window, cx| {
                            // One item per row of a turn: only visible rows are
                            // built and laid out per frame, so per-frame cost
                            // stays bounded by what is on screen, not by the
                            // size of the turns on screen. The 8 px gap between
                            // a turn's blocks is the design's `.grp2{gap:8px}`;
                            // the 16 px below each turn's last row is the
                            // transcript column's `.tr{gap:16px}`.
                            let Some(&(turn_ix, row)) = rows.get(ix) else {
                                return div().into_any_element();
                            };
                            let Some(turn) = turns.get(turn_ix) else {
                                return div().into_any_element();
                            };
                            let last_of_turn = ix == last_row || rows.get(ix + 1).is_some_and(|(t, _)| *t != turn_ix);
                            let mut item = div().w_full().pb(px(if last_of_turn { scale::SP_5 } else { scale::SP_3 }));
                            if ix == 0 {
                                item = item.pt(px(TRANSCRIPT_PAD_TOP));
                            }
                            // Under the deterministic flag every turn draws settled:
                            // the newest turn's reveal (fade + rise) never lands on
                            // the same frame twice.
                            let settled = turn_ix != last_turn || crate::clock::deterministic();
                            item.child(transcript::turn_row(turn, row, settled, &folds, window, cx)).into_any_element()
                        })
                        .flex_1()
                        .into_any_element(),
                    )
            )
            .into_any_element()
    }

    /// The centre while history is still paging in: an empty pane that
    /// keeps its height, so the composer stays docked and the status row
    /// above it ("Loading history…", `render_status`) is the one indicator.
    /// Never the "New session" empty state.
    pub(super) fn loading_row() -> AnyElement {
        div().w_full().flex_1().min_h(px(0.0)).into_any_element()
    }

    /// Re-snapshot what `render_transcript` reads every frame: the turn list
    /// and the truncated-output map.
    ///
    /// The fold owns the fetch handle (`outputRef`); this view owns the
    /// result. Called when the fold changes (length drift or `follow`), never
    /// per frame, so steady-state frames share one `Rc`.
    pub(super) fn refresh_render_cache(&mut self) {
        if let Some(session) = self.fold.session(&self.session_id) {
            // Only the turns that changed are copied. The rest hand back the
            // `Rc` the previous snapshot already held, so a streaming chunk
            // costs one turn's clone rather than the transcript's
            // (finding `performance-4`). The comparison is what makes it
            // safe without a dirty list: an unequal turn is always re-cloned,
            // whatever produced the change.
            let mut held: HashMap<&str, &Rc<Turn>> = HashMap::with_capacity(self.cached_turns.len());
            for turn in self.cached_turns.iter() {
                held.insert(turn.id(), turn);
            }
            let turns: Vec<Rc<Turn>> = session
                .turns
                .iter()
                .map(|turn| match held.get(turn.id()) {
                    Some(previous) if ***previous == *turn => Rc::clone(previous),
                    _ => Rc::new(turn.clone()),
                })
                .collect();
            self.cached_turns = Rc::new(turns);
        }
        // The list's rows, one per block, and the per-turn counts the next
        // `sync_virtual_list` diffs against.
        let mut rows = Vec::with_capacity(self.rows.len());
        let mut row_counts = Vec::with_capacity(self.cached_turns.len());
        for (turn_ix, turn) in self.cached_turns.iter().enumerate() {
            let n = transcript::turn_rows(turn);
            rows.extend((0..n).map(|row| (turn_ix, row)));
            row_counts.push((turn.id().to_owned(), n));
        }
        self.rows = Rc::new(rows);
        self.row_counts = row_counts;
        let mut full_output = HashMap::new();
        // The newest pending approval falls out of the same walk (finding
        // `performance-2`): forward order, keeping the last hit, is the same
        // block the old per-frame reverse scan found first.
        let mut pending: Option<(String, Vec<aui_protocol::ApprovalChoice>)> = None;
        for turn in self.cached_turns.iter() {
            for block in turn.blocks() {
                if let Block::Approval { id, state, choices, .. } = block {
                    if *state == aui_protocol::ApprovalState::Pending {
                        pending = Some((id.clone(), choices.clone()));
                    }
                }
                if let Block::ToolCall { id, body: aui_protocol::ToolBody::Shell { .. }, .. } = block
                {
                    if self.fold.stored_output(&self.session_id, id).is_some() {
                        let state = match self.full_outputs.get(id) {
                            Some(full_output::Fetch::Fetching) => FullOutputState::Fetching,
                            Some(full_output::Fetch::Ready { lines, capped }) => {
                                FullOutputState::Ready { lines: lines.clone(), capped: *capped }
                            }
                            None => FullOutputState::Idle,
                        };
                        full_output.insert(id.clone(), FullOutput { fetchable: true, state });
                    }
                }
            }
        }
        self.cached_full_output = Rc::new(full_output);
        self.cached_pending_approval = pending;
    }

    /// Flip one card's fold override (C8: group headers and per-call cards
    /// share this, keyed stably).
    pub(super) fn toggle_fold(&mut self, key: String, cx: &mut Context<Self>) {
        let toggled = Rc::make_mut(&mut self.toggled);
        if !toggled.remove(&key) {
            toggled.insert(key);
        }
        cx.notify();
    }

    /// A markdown link click (C5): URLs open in the browser, paths resolve
    /// against the session workspace.
    pub(super) fn handle_link(&mut self, target: LinkTarget, cx: &mut Context<Self>) {
        match target {
            LinkTarget::Url(url) => cx.open_url(&url),
            LinkTarget::Path(path) => self.reveal_workspace_path(&path, cx),
        }
    }

    /// Open a linked path in its default place (C5): a folder opens in
    /// Finder, a file in its default app. Resolve against the workspace,
    /// reject escapes above it, toast when nothing is there.
    pub(super) fn reveal_workspace_path(&mut self, raw: &str, cx: &mut Context<Self>) {
        // A trailing `:line` is a viewer hint, not part of the path.
        let path_part = raw.split(':').next().unwrap_or(raw);
        let workspace = PathBuf::from(&self.workspace);
        match resolve_workspace_path(&workspace, raw) {
            Err(_) => {
                self.toast("Link", "That path escapes the session workspace.", cx);
            }
            Ok(path) => match std::fs::metadata(&path) {
                Ok(_) => cx.open_with_system(&path),
                Err(_) => self.toast("Link", format!("No such file: {path_part}"), cx),
            },
        }
    }

    /// An assistant turn's bottom-row action (C6): Copy is local; Retry
    /// resends the user input behind the turn; Fork opens the turn picker.
    /// Pin is hidden on turns (it lives on sidebar sessions) and its arm is
    /// unreachable; the match keeps it because the enum demands it. Wire
    /// actions are live-only — replay answers with a toast.
    pub(super) fn assistant_action(
        &mut self,
        turn_id: String,
        action: AssistantTurnAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = window;
        match action {
            AssistantTurnAction::Copy => {
                let text = self.assistant_text(&turn_id).unwrap_or_default();
                if !text.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }
            AssistantTurnAction::Retry => {
                if self.replay {
                    self.toast("Replay", "Retry is not available in a replayed capture.", cx);
                    return;
                }
                match self.input_before(&turn_id) {
                    Some(text) => self.submit(text, cx),
                    None => self.toast("Retry", "There is no remembered input behind this turn.", cx),
                }
            }
            AssistantTurnAction::Fork => {
                if self.replay {
                    self.toast("Replay", "Fork is not available in a replayed capture.", cx);
                    return;
                }
                cx.emit(SessionEvent::ForkPicker);
            }
            AssistantTurnAction::Pin => {
                // Unreachable: turns hide Pin (see `transcript::block`). Kept
                // for the exhaustive match, answering in case it ever fires.
                self.toast("Pin", "Pin lives on sidebar sessions, not on turns.", cx);
            }
        }
    }

    /// A user turn's bottom-row action (C6): Copy is local, Edit drops the
    /// text into the composer draft, Resend sends it again (live only).
    pub(super) fn user_action(
        &mut self,
        turn_id: String,
        text: String,
        action: aui::transcript::UserTurnAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = turn_id;
        match action {
            aui::transcript::UserTurnAction::Copy => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            aui::transcript::UserTurnAction::Edit => {
                self.set_draft(text, window, cx);
                self.focus_composer(window, cx);
            }
            aui::transcript::UserTurnAction::Resend => {
                if self.replay {
                    self.toast("Replay", "Resend is not available in a replayed capture.", cx);
                    return;
                }
                self.submit(text, cx);
            }
        }
    }

    /// A tool group's intents (C8): the header toggles the group, per-call
    /// toggles flip that call's card, and OpenInPane reveals the call's
    /// target path where it names one.
    pub(super) fn tool_group_action(
        &mut self,
        key: String,
        intent: ToolGroupIntent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match intent {
            ToolGroupIntent::Toggle => self.toggle_fold(key, cx),
            ToolGroupIntent::Call { index, intent } => match intent {
                ToolCardIntent::OpenInPane => self.reveal_tool_target(&key, index, cx),
                _ => self.toggle_fold(format!("{key}:{index}"), cx),
            },
        }
    }

    /// A grouped call's header target, back to the text the lone card would
    /// have shown. The group key is `<turn id>:<block index>`.
    pub(super) fn tool_call_target(&self, turn_id: &str, block_index: usize, call_index: usize) -> Option<String> {
        let session = self.fold.session(&self.session_id)?;
        session.turns.iter().find_map(|turn| match turn {
            Turn::Assistant { id, blocks, .. } if id == turn_id => match blocks.get(block_index) {
                Some(Block::ToolGroup { calls, .. }) => {
                    calls.get(call_index).map(|call| call.target.clone())
                }
                _ => None,
            },
            _ => None,
        })
    }

    /// Reveal a grouped call's target (C5 on grouped cards).
    pub(super) fn reveal_tool_target(&mut self, key: &str, index: usize, cx: &mut Context<Self>) {
        let (turn_id, block_index) = key.rsplit_once(':').unwrap_or((key, ""));
        let block_index = block_index.parse::<usize>().unwrap_or(usize::MAX);
        match self.tool_call_target(turn_id, block_index, index) {
            Some(target) => self.reveal_workspace_path(&target, cx),
            None => self.toast("Open", "That call has no path to reveal.", cx),
        }
    }

    /// An assistant turn's prose, for Copy: every text block joined.
    pub(super) fn assistant_text(&self, turn_id: &str) -> Option<String> {
        let session = self.fold.session(&self.session_id)?;
        session.turns.iter().find_map(|turn| match turn {
            Turn::Assistant { id, blocks, .. } if id == turn_id => {
                let texts: Vec<&str> = blocks
                    .iter()
                    .filter_map(|block| match block {
                        Block::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                Some(texts.join("\n\n"))
            }
            _ => None,
        })
    }

    /// The user input behind an assistant turn, for Retry: the nearest user
    /// turn above it.
    pub(super) fn input_before(&self, turn_id: &str) -> Option<String> {
        let session = self.fold.session(&self.session_id)?;
        let mut last_user: Option<String> = None;
        for turn in &session.turns {
            match turn {
                Turn::User { text, .. } => last_user = Some(text.clone()),
                Turn::Assistant { id, .. } if id == turn_id => return last_user,
                _ => {}
            }
        }
        None
    }

    /// A turn's selection intent (C8b): a drag or a word/paragraph pick
    /// replaces whatever was held (one cell at a time); a plain click
    /// elsewhere in a cell arrives as `None` and clears that turn. The
    /// turn's markdown source travels with the intent so ⌘C slices the
    /// exact view the person dragged in.
    pub(super) fn set_text_selection(
        &mut self,
        turn_id: String,
        source: String,
        next: Option<TextSelection>,
        cx: &mut Context<Self>,
    ) {
        let changed = match next {
            Some(selection) => {
                let fresh = self
                    .text_selections
                    .get(&turn_id)
                    .map(|(_, held)| held != &selection)
                    .unwrap_or(true);
                let held = Rc::make_mut(&mut self.text_selections);
                held.clear();
                held.insert(turn_id, (source, selection));
                // One cell at a time: the clear above leaves exactly this
                // one, so any fresh intent changed what is held.
                fresh
            }
            None => Rc::make_mut(&mut self.text_selections).remove(&turn_id).is_some(),
        };
        if changed {
            cx.notify();
        }
    }

    /// ⌘C in the transcript context (C8b): copy the held selection, if
    /// any, sliced out of its own turn's markdown source. The binding's own
    /// predicate already excludes the composer and card fields, so this
    /// never steals copy from an editor.
    pub fn copy_selected(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let entry = self.text_selections.values().next().cloned();
        let Some((source, selection)) = entry else { return };
        if let Some(text) = turn_selected_text(&source, &selection) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// Clear the transcript text selections (C8b). Returns whether one was
    /// held, so Escape prefers it over heavier dismissals.
    pub fn clear_selection(&mut self, cx: &mut Context<Self>) -> bool {
        if self.text_selections.is_empty() {
            return false;
        }
        Rc::make_mut(&mut self.text_selections).clear();
        cx.notify();
        true
    }

    /// `--steps select-text:<turn>:<from>-<to>`: hold a scripted selection
    /// over the turn's first paragraph (`p0`), for the selection screenshot.
    /// `<turn>` is the turn's index in the live transcript; the range is
    /// byte offsets, clamped to the paragraph. A turn with no text paragraph
    /// (or a bad range) holds nothing rather than a lie.
    pub(crate) fn select_text_step(&mut self, rest: &str, cx: &mut Context<Self>) {
        let (turn, range) = rest.split_once(':').unwrap_or((rest, ""));
        let (from, to) = range.split_once('-').unwrap_or((range, ""));
        let (Ok(index), Ok(mut from), Ok(mut to)) =
            (turn.parse::<usize>(), from.parse::<usize>(), to.parse::<usize>())
        else {
            return;
        };
        if from > to {
            std::mem::swap(&mut from, &mut to);
        }
        // The turn's own markdown source: a user turn is one view, an
        // assistant turn's first text block is the `p0` this step holds.
        let found = self.fold.session(&self.session_id).and_then(|session| {
            session.turns.get(index).and_then(|turn| match turn {
                Turn::User { id, text, .. } => Some((id.clone(), text.clone())),
                Turn::Assistant { id, blocks, .. } => blocks.iter().find_map(|block| match block {
                    Block::Text { text, .. } => Some((id.clone(), text.clone())),
                    _ => None,
                }),
            })
        });
        let Some((turn_id, source)) = found else { return };
        // Clamp to the source so the highlight never addresses bytes that
        // are not there; an emptied range holds nothing rather than a lie.
        from = from.min(source.len());
        to = to.min(source.len());
        if from >= to {
            return;
        }
        let selection = TextSelection { cell: SelectionKey::paragraph("", 0), range: from..to };
        self.set_text_selection(turn_id, source, Some(selection), cx);
    }

    /// Open every tool group for a screenshot: group keys default closed, so
    /// marking them toggled opens them; calls default open.
    pub(crate) fn expand_all_groups(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.fold.session(&self.session_id).cloned() else {
            return;
        };
        for turn in &session.turns {
            let Turn::Assistant { id, blocks, .. } = turn else {
                continue;
            };
            for (index, block) in blocks.iter().enumerate() {
                if matches!(block, Block::ToolGroup { .. }) {
                    Rc::make_mut(&mut self.toggled).insert(transcript::block_key(id, index));
                }
            }
        }
        cx.notify();
    }

    /// Everything the pending approval and question cards need to talk back.
    ///
    /// One struct built once a frame: every closure here is a `cx.listener`, so
    /// a click on a card and a keystroke on the same card run the same code.
    pub(super) fn card_intents(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Cards {
        let choose = cx.listener(|this: &mut Self, (approval, choice, feedback): &(String, String, Option<String>), _, cx| {
            this.decide_approval(approval.clone(), choice.clone(), feedback.clone(), cx);
        });
        let feedback_toggle = cx.listener(|this: &mut Self, (approval, choice): &(String, Option<String>), window, cx| {
            this.toggle_feedback(approval.clone(), choice.clone(), window, cx);
        });
        let select = cx.listener(|this: &mut Self, (id, index): &(String, usize), _, cx| {
            this.select_option(id.clone(), *index, cx);
        });
        let toggle_preview = cx.listener(|this: &mut Self, (id, index): &(String, usize), _, cx| {
            this.toggle_preview(id.clone(), *index, cx);
        });
        let answer = cx.listener(|this: &mut Self, id: &String, _, cx| this.answer_question(id.clone(), cx));
        let skip = cx.listener(|this: &mut Self, id: &String, _, cx| this.skip_question(id.clone(), cx));
        let clarify = cx.listener(|this: &mut Self, id: &String, window, cx| {
            this.clarify_question(id.clone(), window, cx);
        });
        let retry = cx.listener(|this: &mut Self, id: &String, _, cx| this.retry_turn(id.clone(), cx));
        // The two text fields are the app's, exactly as the composer's editor
        // is: the cards are handed an element and never a character.
        let feedback_slot = self.feedback_open.is_some().then(|| {
            Textarea::new(&self.feedback).text_size(aui_tokens::scaled(scale::FS_12)).into_any_element()
        });
        let clarify_slot = self.clarify_open.is_some().then(|| {
            Textarea::new(&self.clarify).text_size(aui_tokens::scaled(scale::FS_12)).into_any_element()
        });
        let feedback_text = self.feedback.read(cx).value().to_string();
        let _ = window;
        Cards {
            choose: Rc::new(move |a, c, f, window, cx| choose(&(a, c, f), window, cx)),
            feedback_toggle: Rc::new(move |a, c, window, cx| feedback_toggle(&(a, c), window, cx)),
            feedback_open: self.feedback_open.clone(),
            feedback_slot: std::cell::RefCell::new(feedback_slot),
            feedback_text,
            select: Rc::new(move |id, index, window, cx| select(&(id, index), window, cx)),
            selections: self.selections.clone(),
            toggle_preview: Rc::new(move |id, index, window, cx| toggle_preview(&(id, index), window, cx)),
            previews: self.previews.clone(),
            answer: Rc::new(move |id, window, cx| answer(&id, window, cx)),
            skip: Rc::new(move |id, window, cx| skip(&id, window, cx)),
            clarify: Rc::new(move |id, window, cx| clarify(&id, window, cx)),
            clarify_open: self.clarify_open.clone(),
            clarify_slot: std::cell::RefCell::new(clarify_slot),
            countdowns: self.countdowns(),
            retry: Rc::new(move |id, window, cx| retry(&id, window, cx)),
            retryable_turns: self.retryable_turns(),
        }
    }

    /// The live status line: history loading, or a running turn with its
    /// elapsed time and the interrupt hint.
    pub(super) fn render_status(&self) -> Option<AnyElement> {
        // A scheduled retry outranks "Working…": the turn is not working, it is
        // waiting out a backoff, and saying which is the whole point of the row.
        if let Some((attempt, max, remaining_ms, reason)) = self.retry_countdown() {
            return Some(
                h_flex()
                    .w_full()
                    .px(px(TRANSCRIPT_PAD_X))
                    .pb(px(scale::SP_4))
                    .child(centred(retry_row("retry", attempt, max, remaining_ms, reason)))
                    .into_any_element(),
            );
        }
        let row = if self.loading_history {
            status_row("status", "Loading history\u{2026}").lead(StatusLead::Spinner).shimmer(true)
        } else if self.busy() {
            let elapsed = self.running.as_ref().map(|r| crate::clock::elapsed_since(r.started).as_millis() as u64).unwrap_or(0);
            // The reply arrived whole but the turn is still open: Muse is on
            // the `reminderChild` tail (memory reminders), not stuck.
            let finishing = self.reply_complete_for_running_turn();
            let mut row = status_row("status", if finishing { "Finishing up\u{2026}" } else { "Working\u{2026}" })
                .lead(StatusLead::Braille)
                .shimmer(true)
                .key_hint("esc", "to interrupt");
            if elapsed > 0 {
                row = row.elapsed(transcript::elapsed(elapsed));
            }
            let queued = self.fold.side(&self.session_id).map(|s| s.queued.len()).unwrap_or(0);
            let note = match (finishing, queued) {
                (true, 0) => Some("memory reminders".to_owned()),
                (true, queued) => Some(format!("memory reminders · {queued} queued")),
                (false, 0) => None,
                (false, queued) => Some(format!("{queued} queued")),
            };
            if let Some(note) = note {
                row = row.note(note);
            }
            row
        } else {
            return None;
        };
        Some(
            h_flex()
                .w_full()
                .px(px(TRANSCRIPT_PAD_X))
                .pb(px(scale::SP_4))
                .child(centred(row))
                .into_any_element(),
        )
    }

    /// The inline banner over the composer, for a recoverable command error.
    ///
    /// Its action is the error's own way out where there is one — "Retry" for a
    /// wire that said "not now" — and a plain dismiss otherwise.
    pub(super) fn render_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let message = self.banner.clone()?;
        let label = match self.banner_action {
            Some(_) => "Retry",
            None => "Dismiss",
        };
        let press = cx.listener(|this: &mut Self, _: &(), _, cx| this.run_banner_action(cx));
        Some(
            div()
                .w_full()
                .px(px(TRANSCRIPT_PAD_X))
                .pb(px(scale::SP_3))
                .child(centred(
                    banner("session-banner", BannerKind::Error, vec![BannerRun::Text(message.into())])
                        .action(label, BannerActionStyle::Ghost)
                        .on_action(move |window, cx| press(&(), window, cx)),
                ))
                .into_any_element(),
        )
    }

    /// The billing guard's banner (Phase 5 A1), directly over the composer
    /// because it is about the thing the composer is for.
    ///
    /// Pay-as-you-go is `Waiting`-tinted and carries both the way out and the
    /// way through; an unknown plan is a quiet `Info` line that blocks nothing.
    pub(super) fn render_tier_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let guard = self.tier_banner.clone()?;
        let sign_out = cx.listener(|_: &mut Self, _: &(), _, cx| cx.emit(SessionEvent::Logout));
        let through = cx.listener(move |_: &mut Self, _: &(), _, cx| {
            cx.emit(SessionEvent::TierOverride);
        });
        let recheck = cx.listener(|_: &mut Self, _: &(), _, cx| cx.emit(SessionEvent::TierRecheck));
        let kind = if guard.blocking { BannerKind::Waiting } else { BannerKind::Info };
        let mut row = banner("tier-banner", kind, vec![BannerRun::Text(guard.text.clone().into())]);
        row = if guard.blocking {
            row.secondary_action("Sign out", BannerActionStyle::Ghost)
                .on_secondary(move |window, cx| sign_out(&(), window, cx))
                .action("Send anyway", BannerActionStyle::Secondary)
                .on_action(move |window, cx| through(&(), window, cx))
        } else {
            row.action("Check again", BannerActionStyle::Ghost)
                .on_action(move |window, cx| recheck(&(), window, cx))
        };
        Some(
            div()
                .w_full()
                .px(px(TRANSCRIPT_PAD_X))
                .pb(px(scale::SP_3))
                .child(centred(row))
                .into_any_element(),
        )
    }

    /// The needs-you banner: something is waiting on the person and they are
    /// not looking at it.
    ///
    /// Only when the pending card is actually out of view — a banner pointing at
    /// a card the reader is already reading is noise.
    pub(super) fn render_needs_you(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (approvals, questions) = self.waiting_on_you()?;
        let at_tail = self.list_state.is_scrolled_to_end().unwrap_or(true);
        if at_tail {
            return None;
        }
        let detail = match (approvals, questions) {
            (a, 0) => format!("{a} approval{} above.", plural(a)),
            (0, q) => format!("{q} question{} above.", plural(q)),
            (a, q) => format!("{a} approval{} and {q} question{} above.", plural(a), plural(q)),
        };
        let jump = cx.listener(|this: &mut Self, _: &(), _, cx| {
            this.list_state.scroll_to_end();
            cx.notify();
        });
        let banner = needs_you_banner("needs-you", "Muse is waiting for you.", detail);
        let banner = if crate::clock::deterministic() { banner.at_rest() } else { banner };
        Some(
            div()
                .w_full()
                .px(px(TRANSCRIPT_PAD_X))
                .pb(px(scale::SP_3))
                .child(centred(banner.on_jump(move |_, window, cx| jump(&(), window, cx))))
                .into_any_element(),
        )
    }

    /// The queued strip: exactly what `SideState::queued` holds, in server
    /// order.
    pub(super) fn render_queue(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let side = self.fold.side(&self.session_id)?;
        if side.queued.is_empty() {
            return None;
        }
        let editing = self.unqueueing.clone();
        let rows: Vec<QueueStripRow> = side
            .queued
            .iter()
            .map(|q| {
                let row = QueueStripRow::new(q.turn_id.clone(), side.queued_text(q).to_owned());
                if editing.get(&q.turn_id) == Some(&Unqueue::Edit) {
                    row.editing()
                } else {
                    row
                }
            })
            .collect();
        let intent = cx.listener(|this: &mut Self, (id, intent): &(SharedString, QueueIntent), _, cx| {
            let why = match intent {
                QueueIntent::Edit => Unqueue::Edit,
                QueueIntent::Remove => Unqueue::Remove,
                QueueIntent::Steer => Unqueue::Steer,
            };
            this.unqueue(id.as_ref(), why, cx);
        });
        Some(
            div()
                .w_full()
                .px(px(TRANSCRIPT_PAD_X))
                .pb(px(scale::SP_3))
                .child(centred(queue_strip("queue", rows).on_intent(move |id, i, window, cx| {
                    intent(&(id.clone(), i), window, cx)
                })))
                .into_any_element(),
        )
    }

    /// The `/` menu and the `@` picker, floating above the composer.
    pub(super) fn render_caret_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let overlays = self.overlays.read(cx);
        let menu = overlays.menu.as_ref()?;
        let (kind, selected, filter) = (menu.kind, menu.selected, menu.filter.clone());
        let element = match kind {
            MenuKind::Command => {
                let (commands, skill_rows) = self.command_rows(&filter, cx);
                let mut sections = Vec::new();
                if !commands.is_empty() {
                    sections.push(CommandSection::new(
                        "Commands",
                        commands
                            .iter()
                            .map(|c| CommandItem::new(c.slash(), c.slash(), c.description()))
                            .collect(),
                    ));
                }
                if !skill_rows.is_empty() {
                    sections.push(CommandSection::new(
                        "Skills",
                        skill_rows
                            .iter()
                            .map(|s| {
                                CommandItem::new(s.id.clone(), format!("/{}", s.name), s.summary())
                                    .source_tag(s.scope().to_owned())
                            })
                            .collect(),
                    ));
                }
                if sections.is_empty() {
                    return None;
                }
                let pick = cx.listener(|this: &mut Self, id: &SharedString, window, cx| {
                    this.select_command(id, window, cx);
                });
                let menu = command_menu("command-menu", format!("/{filter}"), sections, selected);
                let menu = if crate::clock::deterministic() { menu.at_rest() } else { menu };
                menu.on_select(move |id, window, cx| pick(id, window, cx)).into_any_element()
            }
            MenuKind::Mention => {
                let rows = self.mention_rows(&filter, cx);
                if rows.is_empty() {
                    return None;
                }
                let items: Vec<MentionItem> = rows
                    .iter()
                    .map(|path| {
                        let name = path.rsplit('/').next().unwrap_or(path).to_owned();
                        MentionItem::new(path.clone(), MentionIcon::Glyph(IconName::File), name, path.clone())
                            .detail_mono()
                            .matching(&filter)
                    })
                    .collect();
                let pick = cx.listener(|this: &mut Self, id: &SharedString, window, cx| {
                    let insertion = format!("@{id} ");
                    this.replace_token(&insertion, window, cx);
                });
                let picker = mention_picker("mention-picker", filter.clone(), vec![MentionSection::new("Files", items)], selected);
                let picker = if crate::clock::deterministic() { picker.at_rest() } else { picker };
                picker.on_select(move |id, window, cx| pick(id, window, cx)).into_any_element()
            }
            // The chip pickers are anchored to their chips, and the shell's
            // menus to their own buttons — none of them hangs off the caret.
            MenuKind::Model | MenuKind::Effort | MenuKind::Mode => return None,
            MenuKind::Overflow | MenuKind::ViewOptions | MenuKind::Account | MenuKind::Project => return None,
        };
        Some(
            popover_layer(
                div()
                    .id("caret-popover")
                    .absolute()
                    .bottom(px(POPOVER_GAP))
                    .left(px(0.0))
                    .right(px(0.0))
                    .max_h(px(POPOVER_MAX_H))
                    .overflow_y_scroll()
                    .child(element),
            )
            .into_any_element(),
        )
    }

    /// The `/` menu's two sections, filtered by what has been typed.
    pub(super) fn command_rows(&self, filter: &str, cx: &gpui::App) -> (Vec<Command>, Vec<skills::Skill>) {
        let needle = filter.to_lowercase();
        let commands: Vec<Command> = Command::ALL
            .into_iter()
            .filter(|c| c.slash().trim_start_matches('/').to_lowercase().starts_with(&needle))
            .collect();
        let skill_rows: Vec<skills::Skill> = self
            .overlays
            .read(cx)
            .skills_for(&self.workspace)
            .iter()
            .filter(|s| s.name.to_lowercase().starts_with(&needle))
            // F7: a skill whose name is already a client command is hidden.
            // Muse ships `plan`, and the menu offering both `/plan` the mode and
            // `/plan` the skill — which do different things — was a trap.
            .filter(|s| !Command::ALL.iter().any(|c| c.slash().trim_start_matches('/') == s.name))
            .take(SKILL_ROWS)
            .cloned()
            .collect();
        (commands, skill_rows)
    }

    /// The `@` picker's candidates, from the last background rank.
    ///
    /// The rank itself runs in [`SessionView::refresh_mentions`] off the UI
    /// thread; this only reads the cache, so render-adjacent code never scans
    /// 5 000 paths per keystroke. An empty query is the head of the walk
    /// order, which is a slice, not a rank. Everything is keyed by this
    /// session's root: a rank for another root's files never shows here.
    pub(super) fn mention_rows(&self, filter: &str, cx: &gpui::App) -> Vec<String> {
        let overlays = self.overlays.read(cx);
        let files = overlays.files_for(&self.workspace);
        if filter.is_empty() {
            return files.iter().take(files::VISIBLE).map(|entry| entry.path.clone()).collect();
        }
        if self.mention_cache_for == filter
            && self.mention_cache_root == self.workspace
            && self.mention_files_len == files.len()
        {
            return self.mention_cache.clone();
        }
        // A rank is in flight (or not yet started): show the previous rank
        // while it narrows this filter, rather than an empty menu for a frame.
        if !self.mention_cache_for.is_empty()
            && self.mention_cache_root == self.workspace
            && filter.starts_with(&self.mention_cache_for)
        {
            return self.mention_cache.clone();
        }
        Vec::new()
    }

    /// Rank the `@` picker off the UI thread, latest keystroke wins.
    ///
    /// One filter in flight at a time; a rank that finishes after a newer
    /// keystroke started is dropped, so a fast typist never sees a stale list.
    pub(super) fn refresh_mentions(&mut self, filter: String, cx: &mut Context<Self>) {
        if filter.is_empty() {
            return;
        }
        let root = self.workspace.clone();
        let files: Vec<files::FileEntry> = self.overlays.read(cx).files_for(&root).to_vec();
        let files_len = files.len();
        if self.mention_cache_for == filter && self.mention_cache_root == root && self.mention_files_len == files_len {
            return;
        }
        if self.mention_pending.as_deref() == Some(filter.as_str())
            && self.mention_cache_root == root
            && self.mention_files_len == files_len
        {
            return;
        }
        self.mention_epoch += 1;
        let epoch = self.mention_epoch;
        self.mention_pending = Some(filter.clone());
        self.mention_files_len = files_len;
        // The cache says what it was ranked for, exactly: a newer keystroke
        // always starts a newer rank (bumping the epoch), so whatever lands
        // here is either current or dropped above. The root rides along, so a
        // rank for another root's files can never land in this session.
        let wanted = filter.clone();
        let wanted_root = root.clone();
        let work =
            move || files::filter(&files, &filter).into_iter().map(|entry| entry.path.clone()).collect::<Vec<_>>();
        self.wire_call(cx, work, move |this: &mut Self, rows, cx| {
            if this.mention_epoch != epoch {
                return;
            }
            this.mention_pending = None;
            this.mention_cache_for = wanted;
            this.mention_cache_root = wanted_root;
            this.mention_cache = rows;
            cx.notify();
        });
    }

    pub(super) fn render_composer(&self, cx: &mut Context<Self>) -> AnyElement {
        let blocked = self.context().pressure == ContextPressure::Blocked;
        let intent = cx.listener(|this: &mut Self, intent: &ComposerIntent, window, cx| match intent {
            ComposerIntent::Send => this.send(window, cx),
            ComposerIntent::Stop => this.interrupt(cx),
            ComposerIntent::Steer => this.steer(window, cx),
            ComposerIntent::Compact => this.compact(cx),
            ComposerIntent::ExitPlan => this.set_plan(false, cx),
            ComposerIntent::Attach => this.prompt_for_image(cx),
            ComposerIntent::Model => this.toggle_picker(MenuKind::Model, cx),
            ComposerIntent::Effort => this.toggle_picker(MenuKind::Effort, cx),
            ComposerIntent::Mode => this.toggle_picker(MenuKind::Mode, cx),
            ComposerIntent::TogglePlus => {
                this.plus_open = !this.plus_open;
                cx.notify();
            }
            ComposerIntent::RemoveChip(id) => {
                this.images.retain(|image| image.id != id.as_ref());
                this.files.retain(|file| file.id != id.as_ref());
                cx.notify();
            }
        });
        let plus = cx.listener(|this: &mut Self, id: &SharedString, window, cx| {
            this.plus_open = false;
            match id.as_ref() {
                "attach" => this.prompt_for_image(cx),
                "mention" => this.insert_sigil("@", window, cx),
                "commands" => this.insert_sigil("/", window, cx),
                _ => {}
            }
            cx.notify();
        });
        let plus_item = plus_menu(
            "plus",
            vec![
                PlusMenuItem::new("attach", IconName::Paperclip, "Attach file or photo").key("⌘U"),
                PlusMenuItem::new("mention", IconName::At, "@ Mention file"),
                PlusMenuItem::new("commands", IconName::Slash, "/ Slash commands"),
            ],
            self.plus_open,
        )
        .on_activate(move |id, window, cx| plus(id, window, cx));
        let plus_item = if crate::clock::deterministic() { plus_item.at_rest() } else { plus_item };
        let mut element = composer("composer", &self.composer, aui_icons::Provider::Muse, self.model())
            .docked(true)
            .mode(self.mode_label())
            .effort(crate::overlays::effort_label(self.effort))
            .context(self.context())
            .context_open(self.meter_open)
            .plan(self.plan)
            .chips(
                self.images
                    .iter()
                    .map(|image| ComposerChip {
                        id: image.id.clone().into(),
                        kind: ComposerChipKind::Image,
                        label: image.name.clone().into(),
                        removable: true,
                        thumbnail: image.thumb.clone(),
                        detail: None,
                    })
                    .chain(self.files.iter().map(|file| ComposerChip {
                        id: file.id.clone().into(),
                        kind: ComposerChipKind::File,
                        label: file.name.clone().into(),
                        removable: true,
                        thumbnail: None,
                        detail: Some(file.detail().into()),
                    }))
                    .collect(),
            )
            .streaming(self.busy())
            // Blocked context is the server refusing to take more, so the
            // composer refuses too and the meter offers the way out.
            // The draft's emptiness is a maintained flag, not a copy of the
            // whole draft made once a frame to be trimmed (finding
            // `performance-8`).
            .can_send(
                !blocked
                    && !self.attachments_pending()
                    && (!self.draft_empty || !self.images.is_empty() || !self.files.is_empty()),
            )
            .plus_menu(self.plus_open, Some(plus_item))
            .on_intent(move |i, window, cx| intent(&i, window, cx));
        for (anchor, menu) in self.render_pickers(cx) {
            element = element.chip_menu(anchor, menu);
        }
        element.into_any_element()
    }

    /// The three chip pickers, each anchored to the chip that opens it.
    ///
    /// None of them is given an `on_hover`, on purpose: the component already
    /// lets the pointer win the highlight for as long as it is over a row, so an
    /// app that *also* wrote the pointer's row into `selected` would give the
    /// selection two owners — and the check, which marks the session's actual
    /// value, would wander with the mouse. The keyboard owns `selected`; the
    /// pointer owns its own highlight and reports only a click.
    pub(super) fn render_pickers(&self, cx: &mut Context<Self>) -> Vec<(ComposerChipAnchor, AnyElement)> {
        let Some((kind, selected)) = self.overlays.read(cx).menu.as_ref().map(|m| (m.kind, m.selected)) else {
            return Vec::new();
        };
        let close = cx.listener(|this: &mut Self, _: &(), _, cx| this.close_menu(cx));
        match kind {
            MenuKind::Model => {
                let rows: Vec<PickerRow> = self
                    .models
                    .iter()
                    .map(|m| {
                        let mut row = PickerRow::new(
                            m.model_id.clone(),
                            m.display_label.clone(),
                            m.description.clone().unwrap_or_default(),
                        );
                        if let Some(limit) = m.context_limit {
                            row = row.meta(format!("{} ctx", compact_count(limit)));
                        }
                        // A catalog may flag any number of rows either way, so
                        // both badges are drawn wherever they appear.
                        if m.is_default {
                            row = row.badge("default");
                        }
                        if m.is_active {
                            row = row.badge("active");
                        }
                        row
                    })
                    .collect();
                let pick = cx.listener(|this: &mut Self, id: &SharedString, _, cx| {
                    let id = id.to_string();
                    this.pick_model(&id, cx);
                });
                let menu = model_menu("model-menu", rows, selected, true)
                    .on_pick(move |id, window, cx| pick(id, window, cx))
                    .on_close(move |window, cx| close(&(), window, cx));
                let menu = if crate::clock::deterministic() { menu.at_rest() } else { menu };
                vec![(ComposerChipAnchor::Model, menu.into_any_element())]
            }
            MenuKind::Effort => {
                let rows: Vec<PickerRow> = EFFORTS
                    .iter()
                    .map(|effort| {
                        PickerRow::new(
                            effort.map(|e| format!("{e:?}")).unwrap_or_else(|| "default".to_owned()),
                            crate::overlays::effort_label(*effort),
                            crate::overlays::effort_detail(*effort),
                        )
                    })
                    .collect();
                let pick = cx.listener(|this: &mut Self, id: &SharedString, _, cx| {
                    let effort = EFFORTS
                        .iter()
                        .copied()
                        .find(|e| e.map(|e| format!("{e:?}")).unwrap_or_else(|| "default".to_owned()) == id.as_ref());
                    if let Some(effort) = effort {
                        this.pick_effort(effort, cx);
                    }
                });
                let menu = effort_menu("effort-menu", rows, selected, true)
                    .on_pick(move |id, window, cx| pick(id, window, cx))
                    .on_close(move |window, cx| close(&(), window, cx));
                let menu = if crate::clock::deterministic() { menu.at_rest() } else { menu };
                vec![(ComposerChipAnchor::Effort, menu.into_any_element())]
            }
            MenuKind::Mode => {
                let rows: Vec<PickerRow> = MODES
                    .iter()
                    .map(|mode| PickerRow::new(format!("{mode:?}"), mode.label(), mode.description()))
                    .collect();
                let pick = cx.listener(|this: &mut Self, id: &SharedString, _, cx| {
                    if let Some(mode) = MODES.iter().copied().find(|m| format!("{m:?}") == id.as_ref()) {
                        this.pick_mode(mode, cx);
                    }
                });
                let menu = mode_menu("mode-menu", rows, selected, true)
                    .on_pick(move |id, window, cx| pick(id, window, cx))
                    .on_close(move |window, cx| close(&(), window, cx));
                let menu = if crate::clock::deterministic() { menu.at_rest() } else { menu };
                vec![(ComposerChipAnchor::Mode, menu.into_any_element())]
            }
            MenuKind::Command
            | MenuKind::Mention
            | MenuKind::Overflow
            | MenuKind::ViewOptions
            | MenuKind::Account
            | MenuKind::Project => Vec::new(),
        }
    }
}

/// Where a linked path failed to resolve: it escapes the workspace.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Escape {
    /// The normalised path is not under the workspace.
    OutsideWorkspace,
}

/// Resolve a markdown link target to a workspace path, without touching the
/// filesystem: strip the trailing `:line` viewer hint, take an absolute path
/// as is, join a relative one onto the workspace, normalise `..` lexically,
/// and reject anything outside the workspace.
pub(super) fn resolve_workspace_path(workspace: &Path, raw: &str) -> Result<PathBuf, Escape> {
    let path_part = raw.split(':').next().unwrap_or(raw);
    let candidate = if Path::new(path_part).is_absolute() {
        PathBuf::from(path_part)
    } else {
        workspace.join(path_part.trim_start_matches('/'))
    };
    let mut normalized = PathBuf::new();
    for component in candidate.components() {
        match component {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    if !normalized.starts_with(workspace) {
        return Err(Escape::OutsideWorkspace);
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> PathBuf {
        PathBuf::from("/Users/someone/Projects/harness")
    }

    #[test]
    fn an_absolute_path_inside_the_workspace_is_used_as_is() {
        assert_eq!(
            resolve_workspace_path(&workspace(), "/Users/someone/Projects/harness/assets"),
            Ok(PathBuf::from("/Users/someone/Projects/harness/assets"))
        );
    }

    #[test]
    fn a_relative_path_joins_onto_the_workspace() {
        assert_eq!(
            resolve_workspace_path(&workspace(), "assets/logo.png"),
            Ok(PathBuf::from("/Users/someone/Projects/harness/assets/logo.png"))
        );
    }

    #[test]
    fn an_absolute_path_outside_the_workspace_is_rejected() {
        assert_eq!(
            resolve_workspace_path(&workspace(), "/etc/passwd"),
            Err(Escape::OutsideWorkspace)
        );
    }

    #[test]
    fn dotdot_cannot_escape_the_workspace() {
        assert_eq!(
            resolve_workspace_path(&workspace(), "../outside"),
            Err(Escape::OutsideWorkspace)
        );
        assert_eq!(
            resolve_workspace_path(&workspace(), "assets/../../outside"),
            Err(Escape::OutsideWorkspace)
        );
        assert_eq!(
            resolve_workspace_path(&workspace(), "assets/../src/main.rs"),
            Ok(PathBuf::from("/Users/someone/Projects/harness/src/main.rs"))
        );
    }

    #[test]
    fn a_trailing_line_number_is_not_part_of_the_path() {
        assert_eq!(
            resolve_workspace_path(&workspace(), "src/main.rs:12"),
            Ok(PathBuf::from("/Users/someone/Projects/harness/src/main.rs"))
        );
        assert_eq!(
            resolve_workspace_path(&workspace(), "/Users/someone/Projects/harness/src/main.rs:12"),
            Ok(PathBuf::from("/Users/someone/Projects/harness/src/main.rs"))
        );
    }
}
