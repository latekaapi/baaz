//! One frame of the centre pane: the transcript column (cached) and the
//! composer band (live, composed by the application root).
//!
//! Part of [`SessionView`]; see [`crate::session`] for what
//! the entity owns and why these are its own files.

use super::*;
use std::path::Path;
use aui::data::button;
use aui_tokens::ActiveAui;
use crate::providers::{ApprovalChoice, ExternalApproval};

/// The transcript column as its own cached entity:
/// the root embeds the active [`SessionView`] through gpui's `.cached`, so
/// a sidebar-only frame (wheel, reveal, resize tick) reuses the retained
/// transcript instead of rebuilding it. The cache reuses only while the
/// view is clean — every state change that must repaint the transcript
/// notifies the view (streaming deltas, approvals, the loading mark,
/// context pushes in `activate`), while the wire banner, the header and
/// the no-session screen stay inline in the root and paint fresh every
/// frame. Window resize and theme bust through the bounds/style part of
/// the cache key.
///
/// The composer band deliberately lives OUTSIDE this entity, composed live
/// by the application root (`Harness::render_centre`): the library composer
/// embeds its textarea state as a stateful child view, and gpui-base's input
/// element rewrites that state at the end of every paint
/// (`gpui-base-0.6.0/src/input/base/element.rs:2374-2385`). An in-draw notify
/// schedules no frame — gpui-pre only wakes the platform outside the draw
/// phase (`gpui-pre-0.3.3/src/window.rs:167-193`, `invalidate_view`) and
/// clears the dirty set at draw end — so the paint write never self-drives;
/// the band stays out of the cached column anyway, so typing and caret
/// blinks rebuild one row instead of the transcript. The column's own
/// per-tick work is the running animations (status-row braille/shimmer and
/// running activity rows, all infinite while a turn runs) plus the 1 Hz
/// turn ticker — legitimate running work, never an idle loop — so a settled
/// transcript stays clean across sidebar-only frames.
impl gpui::Render for SessionView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        self.render_transcript_column(window, cx)
    }
}

impl SessionView {
    // ----------------------------------------------------------------- render

    /// The transcript column: transcript, status row, banners, queue, caret
    /// menus. What [`gpui::Render`] builds, and what the root caches.
    pub fn render_transcript_column(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        super::note_centre_render();
        let transcript = self.render_transcript(window, cx);
        // A `--screenshot` run that asked for an approval waits for one; this
        // is where the capture learns that it arrived (finding F9). After the
        // transcript, because that is what refreshes the render cache the
        // pending-approval answer is now read from (finding `performance-2`).
        self.capture.set_pending_approval(
            self.newest_pending_approval().is_some() || self.external_approvals.has_pending(),
        );
        let status = self.render_status();
        let capabilities = self.render_capability_strip(cx);
        let external = self.render_external_approvals(window, cx);
        let needs_you = self.render_needs_you(cx);
        let banner = self.render_banner(cx);
        let tier_banner = self.render_tier_banner(cx);
        let queue = self.render_queue(cx);
        let caret_menu = self.render_caret_menu(cx);
        v_flex()
            .size_full()
            .child(transcript)
            .children(status)
            .children(capabilities)
            .children(external)
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
            .into_any_element()
    }

    /// The docked composer band, built live by the application root on every
    /// frame — never inside the cached transcript column (see the
    /// [`gpui::Render`] impl above for why). Cheap next to the transcript:
    /// one textarea element plus closed menus.
    pub fn render_composer_band(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if let Some(text) = self.pending_prompt.take() {
            self.composer.update(cx, |state, cx| state.set_value(text, window, cx));
            self.note_draft(cx);
        }
        let composer = self.render_composer(cx);
        let p = cx.aui().colors;
        // The docked band spans the pane, hairline included; only the
        // composer's content is bound to the measure, as in the design.
        // The library's docked composer draws its own top hairline, which
        // would stop at the measure's edges: the band draws the pane-wide
        // one and the composer is pulled up a pixel so its own lies on it.
        div().w_full().bg(p.surface_1).border_t_1().border_color(p.line).child(
            div()
                .w_full()
                .max_w(px(TRANSCRIPT_MEASURE))
                .mx_auto()
                .mt(px(-1.0))
                .relative()
                .child(composer),
        ).into_any_element()
    }


    /// The file-drop overlay, mounted only while a drag is over the pane:
    /// hidden it still samples its exit presence every render, and a running
    /// presence asks for the next frame — notifying the session view, so the
    /// cached transcript rebuilds on every frame. Absolute positioning means
    /// mounting moves no layout; a deterministic drag capture still draws it
    /// statically through `at_rest`. Composed live by the application root
    /// beside the composer band, so the overlay covers the whole pane.
    pub fn render_drop_overlay(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<AnyElement> {
        self.dragging.then(|| {
            let overlay = aui::composer::drop_overlay("drop", true);
            if crate::clock::deterministic() {
                overlay.at_rest()
            } else {
                overlay
            }
            .into_any_element()
        })
    }

    /// An external drag started moving over the pane: raise the overlay.
    /// Called by the application root's drag listener, which owns the pane
    /// edge the transcript column never sees.
    pub(crate) fn note_drag_over(&mut self, cx: &mut Context<Self>) {
        if !self.dragging {
            self.dragging = true;
            cx.notify();
        }
    }

    /// An external drop landed: take the overlay down and attach the paths.
    pub(crate) fn drop_external(&mut self, paths: &ExternalPaths, cx: &mut Context<Self>) {
        self.dragging = false;
        self.attach_paths(paths.paths().to_vec(), cx);
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
        // One offset per frame. The capture handler only
        // accumulates; this drain is the frame's single `scroll_by`.
        self.drain_pending_wheel();
        // Keep presenting through the tail. A frame every
        // tick while the gesture is open — what `InputRateTracker` does for
        // a second after ≥ 60 inputs/s, but the 60 Hz momentum tail sits on
        // its threshold. A settled transcript requests nothing once it
        // lapses, which is what `bench-idle` asserts.
        if self.gesture_active() {
            window.request_animation_frame();
        }
        self.sync_render_cache();
        if self.cached_turns.is_empty() {
            return self.empty_or_loading(window, cx);
        }
        let folds = self.fold_intents(window, cx);
        let element = self.transcript_list(folds, cx);
        record_frame_stats(frame_start.elapsed());

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
            super::note_centre_loading_paint();
            return Self::loading_row();
        }
        super::note_centre_hero_paint();
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
        // The hero sits above the title, the way the boot screen does. It
        // replaced a small mascot perched on the composer: at 44 pt that read
        // as an ornament stuck to the chrome rather than part of the screen.
        let hero = Some(crate::mascot::hero_mascot(&self.session_id, window, cx));
        transcript::empty_state(self.provider_kind(), &display, hero, pick, cx)
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
            // One clock per frame for every turn age, and the one turn (if
            // any) holding its copy check.
            now_ms: transcript::transcript_now_ms(&self.cached_turns),
            copied: Rc::new(self.copied_turn.iter().cloned().collect()),
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
            // Cross-block spans (C8b): every turn gets its own held span,
            // and every event carries its turn's markdown source back, so
            // the fold slices the exact view the person dragged in.
            span_held: Rc::clone(&self.span_held),
            span_event: {
                let changed = cx.listener(
                    |this: &mut Self,
                     (turn_id, source, event): &(String, String, SpanEvent),
                     _,
                     cx| {
                        this.apply_span(turn_id.clone(), source.clone(), event.clone(), cx);
                    },
                );
                Some(Rc::new(
                    move |turn_id: String,
                          source: String,
                          event: SpanEvent,
                          window: &mut Window,
                          cx: &mut gpui::App| { changed(&(turn_id, source, event), window, cx) },
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
            // D49 play buttons: the request goes to the application as an
            // event, never a turn — honoured under `--replay` like every
            // other local intent here.
            terminal_run: {
                let run = cx.listener(|this: &mut Self, request: &crate::terminal::RunRequest, _, cx| {
                    this.handle_terminal_run(request.clone(), cx);
                });
                Some(Rc::new(
                    move |request: crate::terminal::RunRequest, window: &mut Window, cx: &mut gpui::App| {
                        run(&request, window, cx)
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
    /// 120 Hz budget.
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
        // Never re-anchor under a gesture. While one is in
        // flight no `reset` runs and the tail does not re-engage; whatever
        // is owed lands on the first frame after it lapses.
        let gesture = self.gesture_active();
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
                if gesture {
                    // The count still moves (a splice never resets), but the
                    // re-hint waits: new rows ride no hint for a few frames
                    // rather than paying a `reset`'s dropped wheel events
                    // mid-gesture.
                    self.list_state.splice(from..old, count - from);
                    self.rehint_deferred = true;
                } else {
                    self.rehint_rows(count);
                    self.note_trace_rehint();
                }
            } else {
                self.list_state.splice(from..old, count - from);
            }
        } else if std::mem::take(&mut self.rehint) {
            if gesture {
                self.rehint_deferred = true;
            } else {
                self.rehint_rows(count);
                self.note_trace_rehint();
            }
        }
        if !gesture && std::mem::take(&mut self.rehint_deferred) {
            self.rehint_rows(count);
            self.note_trace_rehint();
        }
        // Tail-follow re-engages only at the end, and never under a gesture:
        // a downward flick the reader started stays theirs until it lapses.
        // A swallowed `follow` is re-raised by the next fold change.
        if std::mem::take(&mut self.follow) && !gesture && self.list_state.is_scrolled_to_end().unwrap_or(true) {
            self.list_state.scroll_to_end();
        }
    }

    /// Give every unmeasured row a hint, keeping measured heights (they
    /// become their own hints) and the scroll position. `reset` drops the
    /// scroll position and the wheel events until the next paint, so the
    /// position is put back by hand; one frame of dropped wheel events is the
    /// price, paid only on a page landing or a width change.
    ///
    /// The hint stays uniform: gpui-pre 0.3.3 exposes no
    /// per-item hint — `ListItem` is a private enum, and `splice` and
    /// `splice_focusable` both build `Unmeasured { size_hint: None }`, so
    /// only `reset_with_uniform_height` can hint at all. Per-kind estimates
    /// would need a gpui fork, which the round rules out.
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
                    // `relative` is load-bearing for `wheel_capture` below:
                    // its canvas is absolute and `size_full`, and an absolute
                    // box resolves against its nearest *positioned* ancestor.
                    // Without a positioned wrapper here that ancestor is the
                    // window, and the transcript's wheel handler would sit
                    // over the sidebar and the composer too — taking their
                    // scroll events and scrolling the transcript instead.
                    .relative()
                    // The list's width, for the re-hint after a change.
                    .on_children_prepainted(move |bounds, _, cx| {
                        if let Some(first) = bounds.first() {
                            let width = f32::from(first.size.width);
                            let _ = width_report.update(cx, |view, _| view.note_list_width(width));
                        }
                    })
                    // D7. The wheel accumulates on the view here, in the
                    // capture phase, and never reaches `list()`'s own handler;
                    // `render_transcript` drains the sum into one `scroll_by`
                    // per frame.
                    //
                    // `list()` accumulates a frame's wheel deltas with
                    // `ScrollDelta::coalesce` and applies the running sum
                    // against the offset it captured at paint. `coalesce`
                    // *overrides* instead of summing when the two signs
                    // differ, and `0.0f32.signum()` is `+1.0` — so a
                    // `scrollingDeltaY == 0.0` sample, which AppKit emits
                    // constantly (the MayBegin/Began pair, a finger-down
                    // pause, the momentum tail), throws away everything
                    // accumulated so far whenever the travel is negative, and
                    // nothing when it is positive. Measured on the 300-turn
                    // capture: a six-event burst with one zero in it kept
                    // 432 px of 864 scrolling up and all 864 scrolling down.
                    // That is the "visibly stepped" scrolling the
                    // measurement showed, and it is asymmetric by direction.
                    //
                    // `scroll_by` is what every other scrollable surface in
                    // the app already does — gpui's `div` adds each event's
                    // own delta to the offset as it arrives — so the
                    // transcript now moves by the same arithmetic as the
                    // sidebar, the menus and the palettes. The hitbox is
                    // gpui's own, so an overlay above the transcript takes the
                    // wheel instead of us; `line_height` matches `div`'s
                    // conversion for a real mouse wheel, where `list()` used a
                    // hardcoded 20 px; and a gesture that is more horizontal
                    // than vertical is left alone, so a wide markdown table
                    // still scrolls sideways.
                    .child(wheel_capture(cx))
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
                                // The trailing spacer below the final block
                                // (P7): the one row no turn owns. Anything
                                // else unresolvable is an empty row, as
                                // before.
                                if turn_ix == turns.len() {
                                    return div()
                                        .w_full()
                                        .h(px(super::TAIL_SPACER_H))
                                        .into_any_element();
                                }
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
        // `sync_virtual_list` diffs against — plus one trailing spacer below
        // the final block, so the last block never sits against the status
        // row or a banner. A real list item (not padding), hinted like any
        // row, so the virtual list's measurement rules hold: it counts in
        // the totals, splices and hints exactly like a block row, and
        // renders as one fixed-height row (`TAIL_SPACER_H`) in
        // `transcript_list`.
        let mut rows = Vec::with_capacity(self.rows.len() + 1);
        let mut row_counts = Vec::with_capacity(self.cached_turns.len() + 1);
        for (turn_ix, turn) in self.cached_turns.iter().enumerate() {
            let n = transcript::turn_rows(turn);
            rows.extend((0..n).map(|row| (turn_ix, row)));
            row_counts.push((turn.id().to_owned(), n));
        }
        rows.push((self.cached_turns.len(), 0));
        row_counts.push((TAIL_SPACER_ID.to_owned(), 1));
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
    /// Finder, a file in its default app. Absolute paths open as is, even
    /// outside the session workspace; relative ones resolve against it.
    /// Only an existing path opens — through the OS default-app / Finder
    /// dispatch, never executed and never created (these paths come from
    /// model output) — and anything else toasts quietly.
    pub(super) fn reveal_workspace_path(&mut self, raw: &str, cx: &mut Context<Self>) {
        // A trailing `:line` is a viewer hint, not part of the path.
        let path_part = raw.split(':').next().unwrap_or(raw);
        let workspace = PathBuf::from(&self.workspace);
        let path = resolve_link_path(&workspace, raw);
        match std::fs::metadata(&path) {
            Ok(_) => cx.open_with_system(&path),
            Err(_) => self.toast("Link", format!("No such file: {path_part}"), cx),
        }
    }

    /// Hold a turn's copy button on its success check for the library's
    /// hold, then clear it. Only one turn holds the check at a time — a
    /// second copy moves it, and the first timer clears nothing that is no
    /// longer its own.
    fn hold_copy(&mut self, turn_id: String, cx: &mut Context<Self>) {
        self.copied_turn = Some(turn_id.clone());
        cx.notify();
        self.tasks.push(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(aui::transcript::COPY_HOLD).await;
            let _ = this.update(cx, |this, cx| {
                if this.copied_turn.as_ref() == Some(&turn_id) {
                    this.copied_turn = None;
                    cx.notify();
                }
            });
        }));
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
                    self.hold_copy(turn_id, cx);
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
        match action {
            aui::transcript::UserTurnAction::Copy => {
                if !text.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    self.hold_copy(turn_id, cx);
                }
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match intent {
            ToolGroupIntent::Toggle => self.toggle_fold(key, cx),
            ToolGroupIntent::Call { index, intent } => match intent {
                ToolCardIntent::OpenInPane => self.reveal_tool_target(&key, index, cx),
                // The library's group cards render no header actions today,
                // so this arm waits for the slot; it resolves the same way
                // lone cards do, so grouped shell calls behave the moment
                // the slot exists.
                ToolCardIntent::Action(_) => self.run_grouped_tool_call(&key, index, window, cx),
                _ => self.toggle_fold(format!("{key}:{index}"), cx),
            },
        }
    }

    /// A play button press (D49): emit it for the application, which pastes
    /// the command into the terminal dock. Local-only by construction —
    /// this is an event, not a submit, so no path here can start a turn or
    /// reach the wire.
    pub(super) fn handle_terminal_run(
        &mut self,
        request: crate::terminal::RunRequest,
        cx: &mut Context<Self>,
    ) {
        cx.emit(SessionEvent::RunInTerminal {
            command: request.command,
            send_enter: request.send_enter,
        });
    }

    /// A grouped shell call's run action (D49): the call's command text
    /// into the dock, like a lone card. Anything else just toggles.
    pub(super) fn run_grouped_tool_call(
        &mut self,
        key: &str,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self
            .tool_call_in_group(key, index)
            .and_then(|call| crate::transcript::shell_run_command(&call))
        {
            Some(command) => {
                if let Some(request) = crate::terminal::RunRequest::new(
                    command,
                    crate::terminal::send_enter_for_alt(window.modifiers().alt),
                ) {
                    self.handle_terminal_run(request, cx);
                }
            }
            None => self.toggle_fold(format!("{key}:{index}"), cx),
        }
    }

    /// A grouped call's header target, back to the text the lone card would
    /// have shown. The group key is `<turn id>:<block index>`.
    pub(super) fn tool_call_target(&self, turn_id: &str, block_index: usize, call_index: usize) -> Option<String> {
        self.tool_call_in_group(&format!("{turn_id}:{block_index}"), call_index).map(|call| call.target)
    }

    /// A grouped call's full shape, back to the fold. The group key is
    /// `<turn id>:<block index>`.
    pub(super) fn tool_call_in_group(&self, key: &str, index: usize) -> Option<aui_protocol::ToolCall> {
        let (turn_id, block_index) = key.rsplit_once(':').unwrap_or((key, ""));
        let block_index = block_index.parse::<usize>().unwrap_or(usize::MAX);
        let session = self.fold.session(&self.session_id)?;
        session.turns.iter().find_map(|turn| match turn {
            Turn::Assistant { id, blocks, .. } if id == turn_id => match blocks.get(block_index) {
                Some(Block::ToolGroup { calls, .. }) => calls.get(index).cloned(),
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

    /// A turn's span event (C8b): folded through the turn's drag session
    /// into the held span. A drag that starts in one paragraph and ends in
    /// another — or in a code block — highlights everything between; a
    /// plain click clears; a new drag clears the old span. The turn's
    /// markdown source travels with the event so ⌘C slices the exact view
    /// the person dragged in. Keyed, not positional, so the span survives
    /// the transcript scrolling mid-drag.
    pub(super) fn apply_span(
        &mut self,
        turn_id: String,
        source: String,
        event: SpanEvent,
        cx: &mut Context<Self>,
    ) {
        let held = Rc::make_mut(&mut self.span_held);
        if super::spans::apply_span_event(held, &mut self.span_sessions, turn_id, source, &event) {
            cx.notify();
        }
    }

    /// ⌘C in the transcript context (C8b): copy the held span, if any,
    /// sliced out of its own turn's markdown source — document order, a
    /// blank line between blocks, list markers kept, code byte-exact. With
    /// no span held this does nothing, exactly as before: the binding's own
    /// predicate already excludes the composer and card fields, so this
    /// never steals copy from an editor.
    pub fn copy_selected(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = super::spans::span_copy_text(&self.span_held) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// Clear the transcript spans (C8b). Returns whether one was held, so
    /// Escape prefers it over heavier dismissals.
    pub fn clear_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let held = Rc::make_mut(&mut self.span_held);
        if super::spans::clear_spans(held, &mut self.span_sessions) {
            cx.notify();
            true
        } else {
            false
        }
    }

    /// The turn's own markdown source for the scripted selection steps: a
    /// user turn is one view, an assistant turn's first text block is the
    /// source the step holds. `None` for a turn with no text to hold.
    fn step_turn_source(&self, index: usize) -> Option<(String, String)> {
        self.fold.session(&self.session_id).and_then(|session| {
            session.turns.get(index).and_then(|turn| match turn {
                Turn::User { id, text, .. } => Some((id.clone(), text.clone())),
                Turn::Assistant { id, blocks, .. } => blocks.iter().find_map(|block| match block {
                    Block::Text { text, .. } => Some((id.clone(), text.clone())),
                    _ => None,
                }),
            })
        })
    }

    /// `--steps select-text:<turn>:<from>-<to>`: hold a scripted selection
    /// over the turn's first paragraph (`p0`), for the selection screenshot.
    /// `<turn>` is the turn's index in the live transcript; the range is
    /// byte offsets, clamped to the paragraph. A turn with no text paragraph
    /// (or a bad range) holds nothing rather than a lie. The hold travels
    /// the span path — a single-cell span renders exactly like the legacy
    /// single-cell selection it replaces — so the verb, its arguments and
    /// its screenshot are unchanged.
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
        let Some((turn_id, source)) = self.step_turn_source(index) else { return };
        // Clamp to the source so the highlight never addresses bytes that
        // are not there; an emptied range holds nothing rather than a lie.
        from = from.min(source.len());
        to = to.min(source.len());
        if from >= to {
            return;
        }
        let single = TextSelection { cell: SelectionKey::paragraph("", 0), range: from..to };
        let span = MessageSelection::from_single(single);
        let held = Rc::make_mut(&mut self.span_held);
        if super::spans::hold_span(held, turn_id, source, Some(span)) {
            cx.notify();
        }
    }

    /// `--steps select-span:<turn>`: hold the whole turn — first non-empty
    /// cell to last — for the cross-block highlight screenshot (a paragraph,
    /// a list and a code block in one drag's span). `<turn>` is the turn's
    /// index in the live transcript; a turn with no selectable text holds
    /// nothing rather than a lie.
    pub(crate) fn select_span_step(&mut self, rest: &str, cx: &mut Context<Self>) {
        let Ok(index) = rest.trim().parse::<usize>() else { return };
        let Some((turn_id, source)) = self.step_turn_source(index) else { return };
        let span = aui::transcript::message_select_all(&source);
        let held = Rc::make_mut(&mut self.span_held);
        if super::spans::hold_span(held, turn_id, source, span) {
            cx.notify();
        }
    }

    /// `--steps copy:<turn>`: press the turn's copy button for the copy-tick
    /// screenshot. `<turn>` is the turn's index in the live transcript, like
    /// `select-span`; it travels the same button arms a press would, so the
    /// hold the capture shows is the hold a person sees.
    pub(crate) fn step_copy(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(index) = rest.trim().parse::<usize>() else { return };
        let Some(session) = self.fold.session(&self.session_id).cloned() else { return };
        let Some(turn) = session.turns.get(index) else { return };
        match turn {
            Turn::User { id, text, .. } => {
                self.user_action(id.clone(), text.clone(), aui::transcript::UserTurnAction::Copy, window, cx);
            }
            Turn::Assistant { id, .. } => {
                self.assistant_action(id.clone(), AssistantTurnAction::Copy, window, cx);
            }
        }
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

    /// The status row's leading words: the most specific phase the fold can
    /// source truthfully, one calm line. A turn blocked on the person (a
    /// pending approval, an unanswered question) says so rather than reading
    /// as working; a running tool names its family; a growing reasoning
    /// trace reads as thinking; otherwise the generic working/finishing pair
    /// the row always had. The timer, the `esc` hint and the queued/memory
    /// notes are unchanged.
    fn status_phase(&self) -> String {
        if let Some(running) = self.running.as_ref() {
            let blocks = self.cached_turns.iter().find(|turn| turn.id() == running.turn_id).and_then(
                |turn| match turn.as_ref() {
                    Turn::Assistant { blocks, .. } => Some(blocks.as_slice()),
                    Turn::User { .. } => None,
                },
            );
            if let Some(blocks) = blocks {
                if let Some(phase) = Self::status_phase_for_blocks(blocks) {
                    return phase;
                }
            }
        }
        if self.reply_complete_for_running_turn() {
            "Finishing up…".to_owned()
        } else {
            "Working…".to_owned()
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
            let mut row = status_row("status", self.status_phase())
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

/// The specific phase for a running turn's blocks, when one reads truthfully
/// off the fold: waiting (a pending approval, an unanswered question)
/// outranks running (a tool in flight names its family), which outranks a
/// growing reasoning trace. `None` is the generic working/finishing pair —
/// nothing here was specific enough to name.
fn status_phase_for_blocks(blocks: &[Block]) -> Option<String> {
    use aui_protocol::{ApprovalState, ThinkingState};
    if blocks.iter().any(|block| matches!(block, Block::Approval { state: ApprovalState::Pending, .. })) {
        return Some("Waiting for approval…".to_owned());
    }
    if blocks.iter().any(|block| matches!(block, Block::Question { answer: None, .. })) {
        return Some("Waiting for your answer…".to_owned());
    }
    if let Some(word) = blocks.iter().find_map(Self::running_tool_word) {
        return Some(format!("Running {word}…"));
    }
    if blocks
        .iter()
        .any(|block| matches!(block, Block::Thinking { state: ThinkingState::Thinking, .. }))
    {
        return Some("Thinking…".to_owned());
    }
    None
}

/// The family word of the first still-running call in `block`, if any: the
/// fold's own kind, never a guessed activity. A group with more than one
/// call in flight reads as tools.
fn running_tool_word(block: &Block) -> Option<String> {
    use aui_protocol::{ActivityState, ToolStatus};
    let open = |status: &ToolStatus| matches!(status, ToolStatus::Pending | ToolStatus::Running);
    match block {
        Block::ToolCall { kind, status, .. } if open(status) => Some(Self::tool_word(kind).to_owned()),
        Block::ToolGroup { calls, state, .. }
            if *state == ActivityState::Working || calls.iter().any(|call| open(&call.status)) =>
        {
            let mut running = calls.iter().filter(|call| open(&call.status));
            match (running.next(), running.next()) {
                (Some(call), None) => Some(Self::tool_word(&call.kind).to_owned()),
                _ => Some("tools".to_owned()),
            }
        }
        _ => None,
    }
}

/// One calm word for a tool family, from the fold's kind. An MCP tool keeps
/// the server's own tool name, verbatim.
fn tool_word(kind: &aui_protocol::ToolKind) -> &str {
    match kind {
        aui_protocol::ToolKind::Shell => "command",
        aui_protocol::ToolKind::Read => "read",
        aui_protocol::ToolKind::Edit => "edit",
        aui_protocol::ToolKind::Write => "write",
        aui_protocol::ToolKind::Search => "search",
        aui_protocol::ToolKind::Web => "web lookup",
        aui_protocol::ToolKind::Browser => "browser task",
        aui_protocol::ToolKind::SubAgent => "subagent",
        aui_protocol::ToolKind::Mcp { tool, .. } => tool,
    }
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

    /// The billing guard's banner, directly over the composer
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
        } else if guard.checking {
            // The probe is already running — pressing again reaches
            // `probe_tier`, which refuses a second probe, so the button only
            // says what is happening.
            row.action("Checking…", BannerActionStyle::Ghost)
                .on_action(move |window, cx| recheck(&(), window, cx))
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
        let banner = needs_you_banner("needs-you", self.provider_kind().waiting_headline(), detail);
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
                if editing.get(&q.turn_id).is_some_and(|pending| pending.kind == Unqueue::Edit) {
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
            // The row's text, captured now: by the time `turn/unqueued`
            // lands the fold may no longer be able to echo it back.
            let text = this
                .fold
                .side(&this.session_id)
                .and_then(|live| {
                    let row = live.queued_turn(id.as_ref())?;
                    Some(live.queued_text(row).to_owned())
                })
                .unwrap_or_default();
            this.unqueue(id.as_ref(), why, text, cx);
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
        // The chip wears the session's own provider mark: a Codex session
        // never shows the Muse "M".
        let mut element = composer("composer", &self.composer, self.provider_kind().icon(), self.model())
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

/// Resolve a markdown link target to a filesystem path, without touching the
/// filesystem: strip the trailing `:line` viewer hint, take an absolute path
/// as is (even outside the session workspace), join a relative one onto the
/// workspace, and normalise `..` lexically. Whether anything opens is the
/// caller's decision: only an existing path ever does.
pub(super) fn resolve_link_path(workspace: &Path, raw: &str) -> PathBuf {
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
    normalized
}

/// The transcript's wheel handler: a zero-size canvas over the list that
/// takes every scroll event in the **capture** phase and accumulates its
/// vertical delta on the view. `render_transcript` drains
/// the sum into exactly one [`gpui::ListState::scroll_by`] per frame, so one
/// frame's work no longer grows with the event rate.
///
/// Capture, not bubble, is the only phase that can win here. `list()`
/// registers its own handler during its paint, and `Interactivity::paint`
/// registers a container's listeners *before* painting its children, so in
/// the bubble phase — which runs in reverse registration order — the list
/// always runs before anything wrapping it. A parent `on_scroll_wheel` plus
/// `stop_propagation` is useless against it. Capture runs in registration
/// order and precedes every bubble handler, so this one gets there first and
/// stops the event before `list()` ever sees it.
///
/// See the call site for why `list()`'s own arithmetic had to go.
fn wheel_capture(cx: &mut Context<SessionView>) -> gpui::AnyElement {
    let view = cx.entity().downgrade();
    gpui::canvas(
        // Prepaint: gpui's own hitbox, so the wheel goes to whatever is
        // actually on top. A palette or menu over the transcript owns the
        // pointer, and `should_handle_scroll` is the same test `list()` and
        // every `div` scroll container use.
        move |bounds, window, _cx| window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal),
        move |_bounds, hitbox, window, _cx| {
            window.on_mouse_event(move |event: &gpui::ScrollWheelEvent, phase, window, cx| {
                if phase != gpui::DispatchPhase::Capture || !hitbox.should_handle_scroll(window) {
                    return;
                }
                let delta = event.delta.pixel_delta(window.line_height());
                // A gesture that is more sideways than not belongs to
                // whatever is under it — a wide markdown table is the one
                // horizontally scrollable thing inside a turn.
                if delta.y.abs() < delta.x.abs() {
                    return;
                }
                // Taken whether or not it moves us: a zero sample that
                // reaches `list()` is exactly what resets its accumulator.
                // The list counts pixels from the top, the wheel counts
                // travel, and they run opposite ways; the drain negates.
                cx.stop_propagation();
                let _ = view.update(cx, |view, cx| {
                    view.push_wheel(delta.y);
                    cx.notify();
                });
            });
        },
    )
    .absolute()
    .size_full()
    .into_any_element()
}

impl SessionView {
    /// The capability strip: what this session's provider cannot do (or
    /// has not proven), with the typed reason beside each gated control.
    ///
    /// This is the point of the seam made visible: the same screen
    /// renders differently per provider — Claude Code shows steering and
    /// interruption as unverified and questions as unavailable-in-prose,
    /// Codex shows steering as native, and muse shows no strip at all.
    /// `None` is the muse answer: nothing gated, nothing shown.
    pub(super) fn render_capability_strip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let kind = self.provider_kind();
        let rows: Vec<(String, String)> = [
            ("Steer into the running turn", self.steer_gate()),
            ("Stop the running turn", self.turn_gate()),
            ("Answer questions", self.questions_gate()),
        ]
        .into_iter()
        .filter_map(|(label, gate)| gate.map(|reason| (label.to_owned(), reason)))
        .collect();
        if rows.is_empty() {
            return None;
        }
        let p = cx.aui().colors;
        Some(
            div()
                .w_full()
                .px(px(TRANSCRIPT_PAD_X))
                .pb(px(scale::SP_3))
                .child(centred(
                    v_flex()
                        .id("capability-strip")
                        .role(gpui::Role::Group)
                        .aria_label(format!("Provider capabilities for {}", kind.label()))
                        .gap(px(scale::SP_1))
                        .child(
                            div()
                                .text_color(p.ink_3)
                                .child(format!("On {} in this session:", kind.label())),
                        )
                        .children(rows.into_iter().enumerate().map(|(index, (label, reason))| {
                            div()
                                .id(format!("capability-row-{index}"))
                                .role(gpui::Role::Label)
                                .aria_label(format!("{label} unavailable: {reason}"))
                                .text_color(p.ink_2)
                                .child(format!("{label} — {reason}"))
                        })),
                ))
                .into_any_element(),
        )
    }

    /// The external approval cards: one per new-provider approval the
    /// server has not resolved yet, on the same surface as the legacy
    /// cards and under the same rule — the card changes only on the
    /// server's notification, never on the press.
    ///
    /// Awaiting cards offer all four answers, with `decline` ("Deny":
    /// no, do something else) and `cancel` ("Deny and stop": no, stop)
    /// as visibly different buttons. A sent card offers no second press:
    /// it reads "sent, waiting for the server".
    pub(super) fn render_external_approvals(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let approvals: Vec<ExternalApproval> =
            self.external_approvals.outstanding().into_iter().cloned().collect();
        if approvals.is_empty() {
            return None;
        }
        let p = cx.aui().colors;
        let cards: Vec<AnyElement> = approvals
            .into_iter()
            .map(|approval| self.render_external_card(&approval, p.ink_2, p.ink_3, cx))
            .collect();
        Some(
            div()
                .w_full()
                .px(px(TRANSCRIPT_PAD_X))
                .pb(px(scale::SP_3))
                .child(centred(
                    v_flex()
                        .id("external-approvals")
                        .role(gpui::Role::Group)
                        .aria_label("Provider approvals waiting")
                        .gap(px(scale::SP_2))
                        .children(cards),
                ))
                .into_any_element(),
        )
    }

    /// One external approval card. Every button carries its accessibility
    /// role and human label on the wrapper in the same change (the button
    /// itself has no aria builder, so the wrapper names it).
    fn render_external_card(
        &self,
        approval: &ExternalApproval,
        ink: gpui::Hsla,
        dim: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut card = v_flex()
            .id(format!("ext-appr-{}", approval.id))
            .role(gpui::Role::Group)
            .aria_label(format!("Approval: {}", approval.headline))
            .gap(px(scale::SP_1))
            .child(div().text_color(dim).child(approval.kind.tag().to_owned()))
            .child(div().text_color(ink).child(approval.headline.clone()))
            .child(div().text_color(dim).child(approval.reason.clone()));
        if let Some(note) = approval.dont_ask_again.clone() {
            card = card.child(
                div()
                    .id(format!("ext-rule-{}", approval.id))
                    .role(gpui::Role::Label)
                    .aria_label(format!("Do not ask again option: {note}"))
                    .text_color(dim)
                    .child(format!("Don't ask again: {note}")),
            );
        }
        match approval.decision_sent.clone() {
            // Sent and waiting: no second press, no ahead-of-server change.
            Some(sent) => card.child(
                div()
                    .id(format!("ext-sent-{}", approval.id))
                    .role(gpui::Role::Label)
                    .aria_label(format!("Decision {sent} sent, waiting for the server"))
                    .text_color(dim)
                    .child(format!("Sent {sent} — waiting for the server.")),
            ),
            None => card.child(
                h_flex()
                    .gap(px(scale::SP_2))
                    .children(ApprovalChoice::all().into_iter().map(|choice| {
                        let approval_id = approval.id.clone();
                        let headline = approval.headline.clone();
                        let mut press = button(
                            format!("ext-{}-{}", approval.id, choice.choice_id()),
                            choice.label(),
                        );
                        press = match choice {
                            ApprovalChoice::Accept | ApprovalChoice::AcceptForSession => {
                                press.primary()
                            }
                            ApprovalChoice::Decline | ApprovalChoice::Cancel => press.danger(),
                        };
                        let press = press.on_click(cx.listener(
                            move |this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                                this.decide_external_approval(
                                    approval_id.clone(),
                                    choice,
                                    None,
                                    cx,
                                );
                            },
                        ));
                        // The wrapper carries the role and the human label;
                        // decline and cancel never share one.
                        div()
                            .id(format!("ext-wrap-{}-{}", approval.id, choice.choice_id()))
                            .role(gpui::Role::Button)
                            .aria_label(format!("{}: {}", choice.label(), headline))
                            .child(press)
                    })),
            ),
        }
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> PathBuf {
        PathBuf::from("/Users/someone/Projects/baaz")
    }

    #[test]
    fn an_absolute_path_inside_the_workspace_is_used_as_is() {
        assert_eq!(
            resolve_link_path(&workspace(), "/Users/someone/Projects/baaz/assets"),
            PathBuf::from("/Users/someone/Projects/baaz/assets")
        );
    }

    #[test]
    fn a_relative_path_joins_onto_the_workspace() {
        assert_eq!(
            resolve_link_path(&workspace(), "assets/logo.png"),
            PathBuf::from("/Users/someone/Projects/baaz/assets/logo.png")
        );
    }

    /// Item 4: an absolute path outside the workspace resolves as is — the
    /// existence check at open time, not the workspace boundary, decides.
    #[test]
    fn an_absolute_path_outside_the_workspace_resolves_as_is() {
        assert_eq!(resolve_link_path(&workspace(), "/etc/passwd"), PathBuf::from("/etc/passwd"));
        assert_eq!(resolve_link_path(&workspace(), "/tmp/scratch/note.txt"), PathBuf::from("/tmp/scratch/note.txt"));
    }

    #[test]
    fn dotdot_normalises_lexically_without_rejection() {
        assert_eq!(resolve_link_path(&workspace(), "../outside"), PathBuf::from("/Users/someone/Projects/outside"));
        assert_eq!(
            resolve_link_path(&workspace(), "assets/../../outside"),
            PathBuf::from("/Users/someone/Projects/outside")
        );
        assert_eq!(
            resolve_link_path(&workspace(), "assets/../src/main.rs"),
            PathBuf::from("/Users/someone/Projects/baaz/src/main.rs")
        );
    }

    fn tool_block(kind: aui_protocol::ToolKind, status: aui_protocol::ToolStatus) -> Block {
        Block::ToolCall {
            id: "call-1".to_owned(),
            kind,
            verb: "Ran".to_owned(),
            target: "npm test".to_owned(),
            status,
            duration_ms: None,
            body: aui_protocol::ToolBody::Shell { output_lines: Vec::new(), exit_code: None, live: true },
            diff_stat: None,
        }
    }

    fn approval_block(state: aui_protocol::ApprovalState) -> Block {
        Block::Approval {
            id: "appr-1".to_owned(),
            tool: "Bash".to_owned(),
            command: "rm -rf /tmp/x".to_owned(),
            reason: "clean up".to_owned(),
            cwd: "/tmp".to_owned(),
            capabilities: Vec::new(),
            scope: aui_protocol::ApprovalScope::ThisCommand,
            state,
            rule: None,
            choices: Vec::new(),
            stages: Vec::new(),
            current_stage: None,
            badges: aui_protocol::ApprovalBadges::default(),
            feedback: None,
            resolved_by: None,
        }
    }

    #[test]
    fn a_pending_approval_outranks_a_running_tool() {
        let blocks = vec![
            tool_block(aui_protocol::ToolKind::Shell, aui_protocol::ToolStatus::Running),
            approval_block(aui_protocol::ApprovalState::Pending),
        ];
        assert_eq!(SessionView::status_phase_for_blocks(&blocks).as_deref(), Some("Waiting for approval…"));
    }

    #[test]
    fn a_running_tool_names_its_family() {
        let blocks = vec![tool_block(aui_protocol::ToolKind::Shell, aui_protocol::ToolStatus::Running)];
        assert_eq!(SessionView::status_phase_for_blocks(&blocks).as_deref(), Some("Running command…"));
        let done = vec![tool_block(aui_protocol::ToolKind::Shell, aui_protocol::ToolStatus::Success)];
        assert_eq!(SessionView::status_phase_for_blocks(&done), None);
    }

    #[test]
    fn a_growing_trace_reads_as_thinking() {
        let blocks = vec![Block::Thinking {
            text: "hmm".to_owned(),
            elapsed_ms: 0,
            summary: None,
            state: aui_protocol::ThinkingState::Thinking,
        }];
        assert_eq!(SessionView::status_phase_for_blocks(&blocks).as_deref(), Some("Thinking…"));
        let settled = vec![Block::Text { text: "done".into(), streaming: false }];
        assert_eq!(SessionView::status_phase_for_blocks(&settled), None);
    }

    #[test]
    fn a_trailing_line_number_is_not_part_of_the_path() {
        assert_eq!(
            resolve_link_path(&workspace(), "src/main.rs:12"),
            PathBuf::from("/Users/someone/Projects/baaz/src/main.rs")
        );
        assert_eq!(
            resolve_link_path(&workspace(), "/Users/someone/Projects/baaz/src/main.rs:12"),
            PathBuf::from("/Users/someone/Projects/baaz/src/main.rs")
        );
    }

    /// Item 4: the existence check behind the open decision — a file inside
    /// the workspace, a directory, and an absolute path outside it all
    /// exist (so they open); only the missing path toasts.
    #[test]
    fn the_existence_check_covers_files_dirs_and_outside_paths() {
        let root = std::env::temp_dir().join(format!("baaz-link-resolve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).expect("probe dir");
        std::fs::write(root.join("sub").join("note.txt"), "hi").expect("probe file");
        assert!(std::fs::metadata(resolve_link_path(&root, "sub/note.txt")).is_ok(), "inside file");
        assert!(std::fs::metadata(resolve_link_path(&root, "sub")).is_ok(), "inside dir");
        assert!(std::fs::metadata(resolve_link_path(&root, "/tmp")).is_ok(), "outside dir");
        assert!(std::fs::metadata(resolve_link_path(&root, "sub/gone.txt")).is_err(), "missing file");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Item 4: a missing path toasts quietly — inside the workspace, outside
    /// it, and with a `:line` hint — instead of opening. (Existing paths
    /// open through the platform dispatch, which the headless platform does
    /// not implement; the resolver tests above pin that outside paths reach
    /// the existence check rather than a workspace rejection.)
    #[gpui::test]
    /// D49: a play button press emits `RunInTerminal` — an event for the
    /// application's terminal dock — and does nothing else. In particular
    /// it never submits: the composer draft is untouched, no toast fires,
    /// and the view holds no client, so no path here can start a turn or
    /// reach the wire. `handle_terminal_run`'s only effect is the emit
    /// below, which is what this pins.
    #[gpui::test]
    fn play_buttons_emit_a_run_never_a_turn(cx: &mut gpui::TestAppContext) {
        use std::cell::RefCell;
        use std::rc::Rc;
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let root = std::env::temp_dir().join(format!("baaz-run-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("probe dir");
        let vc = cx.add_empty_window();
        let seen: Rc<RefCell<Vec<(String, bool)>>> = Rc::new(RefCell::new(Vec::new()));
        // Emitted events flush when the update that raised them ends, so
        // the presses run in one update and the assertions read in the
        // next; the subscription outlives both.
        let (view, overlays, _sub) = vc.update(|window, cx| {
            let overlays = cx.new(|_| crate::overlays::Overlays::default());
            let host = crate::session::SessionHost {
                provider_id: "echo".to_owned(),
                workspace: root.to_string_lossy().into_owned(),
                overlays: overlays.clone(),
                capture: crate::shot::CaptureToken::default(),
            };
            let view = cx.new(|cx| crate::session::SessionView::new("s-1".to_owned(), None, host, window, cx));
            let record = seen.clone();
            let sub = cx.subscribe(&view, move |_, event: &crate::session::SessionEvent, _| {
                match event {
                    crate::session::SessionEvent::RunInTerminal { command, send_enter } => {
                        record.borrow_mut().push((command.clone(), *send_enter));
                    }
                    _ => record.borrow_mut().push((String::from("unexpected event"), false)),
                }
            });
            view.update(cx, |view, cx| {
                view.handle_terminal_run(
                    crate::terminal::RunRequest { command: "npm test".to_owned(), send_enter: true },
                    cx,
                );
                // ⌥-click pastes without Enter: the same press with the
                // modifier held resolves before the emit, above this seam.
                view.handle_terminal_run(
                    crate::terminal::RunRequest { command: "git status".to_owned(), send_enter: false },
                    cx,
                );
            });
            (view, overlays, sub)
        });
        vc.update(|_, cx| {
            assert_eq!(
                *seen.borrow(),
                vec![
                    ("npm test".to_owned(), true),
                    ("git status".to_owned(), false),
                ],
                "two presses, two local run events, in order"
            );
            assert!(view.read(cx).draft_text(cx).is_empty(), "no turn was drafted, let alone sent");
            assert!(overlays.read(cx).toasts.is_empty(), "no toast, no banner, no wire");
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn missing_linked_paths_toast_quietly(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let root = std::env::temp_dir().join(format!("baaz-link-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).expect("probe dir");
        let vc = cx.add_empty_window();
        vc.update(|window, cx| {
            let overlays = cx.new(|_| crate::overlays::Overlays::default());
            let host = crate::session::SessionHost {
                provider_id: "echo".to_owned(),
                workspace: root.to_string_lossy().into_owned(),
                overlays: overlays.clone(),
                capture: crate::shot::CaptureToken::default(),
            };
            let view = cx.new(|cx| crate::session::SessionView::new("s-1".to_owned(), None, host, window, cx));
            let toasts = |cx: &gpui::App| overlays.read(cx).toasts.len();
            view.update(cx, |view, cx| view.reveal_workspace_path("sub/gone.txt", cx));
            view.update(cx, |view, cx| view.reveal_workspace_path("/tmp/definitely-not-here-xyz", cx));
            view.update(cx, |view, cx| view.reveal_workspace_path("sub/gone.txt:12", cx));
            assert_eq!(toasts(cx), 3, "missing paths toast instead of opening");
            for toast in overlays.read(cx).toasts.iter() {
                assert_eq!(toast.title.as_ref(), "Link");
                assert!(
                    toast.body.starts_with("No such file: "),
                    "quiet missing-file toast, got: {}",
                    toast.body
                );
            }
        });
        let _ = std::fs::remove_dir_all(&root);
    }
}
