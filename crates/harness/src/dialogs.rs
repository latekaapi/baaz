//! What floats over the window: the modal, the palette, the toast stack and
//! the header's overflow menu.
//!
//! [`crate::overlays`] owns the state — which modal is up, which menu is open,
//! where its selection sits, what the menus are built from. This module owns
//! the elements for the ones the **window** anchors, and the intents that put
//! them up and take them down. The two the sidebar anchors live in
//! [`crate::sidebar_view`], and the composer's own chip menus in
//! [`crate::session`], because a picker is anchored to the affordance that
//! opened it and only its owner knows where that is.
//!
//! Everything here goes through `aui::overlay::popover_layer`, so the paint
//! order stays the single one the library defines, and a deterministic capture
//! draws the dialog and the toast stack settled rather than rising in: an
//! enter presence never lands on the same frame twice.

use aui::keys::{Cancel, Confirm, SelectNext, SelectPrev};
use aui::nav::{sidebar_search, view_menu, MenuRow};
use aui::overlay::{command_palette, dialog, popover_layer, DialogKind, PaletteIcon, PaletteItem, PaletteSection};
use aui_icons::IconName;
use aui_tokens::scale;
use gpui::{
    div, prelude::*, px, AnyElement, Context, SharedString, Window,
};
use gpui_kit::base::v_flex;
use gpui_kit::component::input::Textarea;

use crate::app::{Harness, Wire, PALETTE_ROWS, PALETTE_SCRIM, PALETTE_TOP, TOAST_STACK_H, TOAST_TOP, TOAST_W};
use crate::login::Auth;
use crate::overlays::{Command, Dialog, DialogAction, Menu, MenuKind, Palette, PaletteKind};
use crate::wire::WireCall;
use crate::{files, skills};

/// Actions of the header's overflow menu, in row order.
#[derive(Clone, Copy)]
enum OverflowAction {
    Rename,
    Fork,
    Archive,
}

/// One palette section's rows under one icon.
fn palette_items(
    rows: &[(SharedString, SharedString, SharedString)],
    icon: PaletteIcon,
) -> Vec<PaletteItem> {
    rows.iter().map(|(id, label, detail)| PaletteItem::new(id.clone(), icon, label.clone()).context(detail.clone())).collect()
}

/// [`palette_items`] over partitioned row references, which is what the search
/// palette's two sections are built from.
fn palette_items_ref(
    rows: &[&(SharedString, SharedString, SharedString)],
    icon: PaletteIcon,
) -> Vec<PaletteItem> {
    rows.iter().map(|(id, label, detail)| PaletteItem::new((*id).clone(), icon, (*label).clone()).context((*detail).clone())).collect()
}

/// [`palette_items_ref`] with the query's first hit in each label emphasised
/// through the row's own `matched` ranges. An empty query emphasises nothing.
fn palette_items_ref_matching(
    rows: &[&(SharedString, SharedString, SharedString)],
    icon: PaletteIcon,
    needle: &str,
) -> Vec<PaletteItem> {
    palette_items_ref(rows, icon)
        .into_iter()
        .map(|item| if needle.trim().is_empty() { item } else { item.matching(needle) })
        .collect()
}

impl Harness {
    /// Put a modal up. Only one at a time, which is what makes Escape's order
    /// (menu, then modal) a single rule.
    pub(crate) fn set_dialog(&mut self, cx: &mut Context<Self>, dialog: Dialog) {
        self.overlays.update(cx, |overlays, _| overlays.dialog = Some(dialog));
        cx.notify();
    }

    pub(crate) fn close_dialog(&mut self, cx: &mut Context<Self>) {
        self.overlays.update(cx, |overlays, _| overlays.dialog = None);
        cx.notify();
    }

    /// The two lists the `/` and `@` menus are built from, walked once at boot
    /// on the background executor and re-walked when a new session starts.
    ///
    /// Neither is on the wire: skills reach MSP only as `toolCall` items, and
    /// a mention is plain text inside the prompt (research §1.5).
    pub(crate) fn load_menu_sources(&mut self, cx: &mut Context<Self>) {
        let program = self.args.program.clone();
        let root = self.args.workspace.clone();
        let work = move || (skills::list(&program), files::walk(&root));
        self.wire_call(cx, work, |this, (skills, files), cx| {
            if files.truncated {
                crate::harness_log!(
                    "@ mention index stopped at {} files; some workspace files are not mentionable",
                    files::CAP
                );
            }
            this.overlays.update(cx, |overlays, _| {
                overlays.skills = skills;
                overlays.files = files.entries;
                overlays.files_truncated = files.truncated;
            });
            cx.notify();
        });
    }

    /// Open the palette on one list.
    pub(crate) fn open_palette(&mut self, kind: PaletteKind, cx: &mut Context<Self>) {
        let already = self.overlays.read(cx).palette.as_ref().is_some_and(|p| p.kind == kind);
        self.overlays.update(cx, |overlays, _| {
            overlays.palette = if already { None } else { Some(Palette { kind, selected: 0 }) };
        });
        cx.notify();
    }

    /// The palette's rows, in the order it draws them, so the keyboard and the
    /// click agree about what row 3 is.
    fn palette_rows(&self, kind: PaletteKind, cx: &gpui::App) -> Vec<(SharedString, SharedString, SharedString)> {
        match kind {
            PaletteKind::Commands => Command::ALL
                .into_iter()
                .map(|c| (c.slash().into(), c.slash().into(), c.description().into()))
                .collect(),
            PaletteKind::Resume => self
                .visible_sessions(cx)
                .iter()
                // Newest first, and only as many as the palette can show: a
                // list taller than the window is a list with a hidden bottom.
                .take(PALETTE_ROWS)
                .map(|entry| {
                    let meta: SharedString =
                        if entry.turns > 0 { format!("{} turns", entry.turns).into() } else { "no turns".into() };
                    (entry.id.clone().into(), entry.label.clone().into(), meta)
                })
                .collect(),
            PaletteKind::Search => self.search_rows(cx),
            // The active session's completed turns, newest first; the rows
            // come from the view because the window does not keep a transcript.
            PaletteKind::Fork => self
                .active
                .as_ref()
                .map(|view| view.read(cx).fork_turns())
                .unwrap_or_default()
                .into_iter()
                .take(PALETTE_ROWS)
                .map(|(id, label, detail)| (id.into(), label.into(), detail.into()))
                .collect(),
        }
    }

    /// Run the palette's selected row: the keyboard's path, which is the one
    /// that still has to resolve an index into a row.
    fn confirm_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((kind, selected)) = self.overlays.read(cx).palette.as_ref().map(|p| (p.kind, p.selected)) else {
            return;
        };
        let rows = self.palette_rows(kind, cx);
        let Some((id, _, _)) = rows.get(selected).cloned() else { return };
        self.run_palette_row(kind, id, window, cx);
    }

    /// Run one palette row by its id.
    ///
    /// A click already knows which row it hit, so it comes straight here
    /// rather than rebuilding every row to turn the id back into an index and
    /// the index back into the same id (finding `performance-7`).
    fn run_palette_row(&mut self, kind: PaletteKind, id: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.overlays.update(cx, |overlays, _| overlays.palette = None);
        match kind {
            PaletteKind::Search => {
                if let Some(session_id) = id.strip_prefix("s:") {
                    self.resume(session_id.to_owned(), window, cx);
                } else if let Some(rest) = id.strip_prefix("f:") {
                    self.reveal_created(rest, cx);
                }
            }
            PaletteKind::Resume => self.resume(id.to_string(), window, cx),
            PaletteKind::Fork => {
                self.with_session(cx, |view, vc| view.fork(Some(id.to_string()), vc));
            }
            PaletteKind::Commands => {
                if let Some(command) = Command::parse(&id) {
                    self.with_session(cx, |view, cx| view.run_command(command, window, cx));
                }
            }
        }
        cx.notify();
    }

    /// Open a header/footer menu, replacing whatever is open. Clicking its
    /// own button again closes it.
    pub(crate) fn open_menu(&mut self, kind: MenuKind, cx: &mut Context<Self>) {
        let already = self.overlays.read(cx).menu.as_ref().is_some_and(|m| m.kind == kind);
        self.overlays.update(cx, |overlays, _| {
            overlays.menu = if already { None } else { Some(Menu::picker(kind, 0)) };
        });
        cx.notify();
    }

    /// The header's overflow menu, anchored under the "…" button: Rename swaps
    /// the title for the dense inline field, Fork opens the fork picker, and
    /// Archive asks first through the archive dialog.
    pub(crate) fn render_overflow_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.overlays.read(cx).is_open(MenuKind::Overflow) {
            return None;
        }
        let rows = vec![
            MenuRow::Toggle { label: "Rename".into(), checked: false },
            MenuRow::Toggle { label: "Fork".into(), checked: false },
            MenuRow::Toggle { label: "Archive".into(), checked: false },
        ];
        let actions = [OverflowAction::Rename, OverflowAction::Fork, OverflowAction::Archive];
        let activate = cx.listener(move |this: &mut Self, index: &usize, window, cx| {
            let action = actions.get(*index).copied();
            this.overlays.update(cx, |overlays, _| overlays.menu = None);
            match action {
                Some(OverflowAction::Rename) => {
                    if let Some(session_id) = this.active_id(cx) {
                        this.sidebar_open = true;
                        this.start_rename(session_id, window, cx);
                    } else {
                        this.overlays.update(cx, |overlays, _| {
                            overlays.toast("Nothing to rename", "No session is open.");
                        });
                    }
                    cx.notify();
                }
                Some(OverflowAction::Fork) => this.open_palette(PaletteKind::Fork, cx),
                Some(OverflowAction::Archive) => {
                    if let Some(session_id) = this.active_id(cx) {
                        this.open_archive_dialog(session_id, cx);
                    } else {
                        this.overlays.update(cx, |overlays, _| {
                            overlays.toast("Nothing to archive", "No session is open.");
                        });
                        cx.notify();
                    }
                }
                None => {}
            }
        });
        Some(
            popover_layer(
                div()
                    .absolute()
                    .top(px(48.0))
                    .right(px(8.0))
                    .child(view_menu("overflow", rows).at_rest().on_activate(move |i, w, cx| activate(&i, w, cx))),
            )
            .into_any_element(),
        )
    }

    /// ↑/↓ in an open menu, wrapping over the rows the menu actually has.
    pub(crate) fn move_menu(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(view) = self.active.clone() else { return };
        let rows = view.read(cx).menu_rows(cx);
        self.overlays.update(cx, |overlays, _| overlays.move_selection(delta, rows));
        cx.notify();
    }

    /// The search palette's query field, above the card. The card's own query
    /// row is display-only, so the palette needs a real field to type in;
    /// clearing it returns to the empty state (recents).
    fn render_search_input(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.overlays.read(cx).palette.as_ref().is_some_and(|p| p.kind == PaletteKind::Search) {
            return None;
        }
        let query = self.search_query.read(cx).value().to_string();
        let clear = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, window, cx| {
            this.search_query.update(cx, |state, cx| state.set_value(String::new(), window, cx));
        });
        Some(
            sidebar_search("palette-search", Textarea::new(&self.search_query).text_size(aui_tokens::scaled(scale::FS_12)))
                .clearable(!query.is_empty())
                .on_clear(clear)
                .into_any_element(),
        )
    }

    /// ⌘K and `/resume`: the command palette, over everything.
    ///
    /// The same primitive for both lists, because they are the same gesture —
    /// a list, an arrow key and a return — and a second picker would be a
    /// second set of keys to learn.
    pub(crate) fn render_palette(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (kind, selected) = self.overlays.read(cx).palette.as_ref().map(|p| (p.kind, p.selected))?;
        let rows = self.palette_rows(kind, cx);
        // The card's own query row mirrors the query for the picking lists;
        // the search palette edits through its own field above the card, so
        // the card's row carries the result count instead.
        let (query, placeholder, sections) = match kind {
            PaletteKind::Commands => (
                SharedString::from(""),
                SharedString::from("Every command in this build"),
                vec![PaletteSection::new(
                    "Commands",
                    palette_items(&rows, PaletteIcon::Glyph(IconName::Slash)),
                )],
            ),
            PaletteKind::Resume => (
                SharedString::from(""),
                SharedString::from("Resume a session in this workspace"),
                vec![PaletteSection::new(
                    "Sessions",
                    palette_items(&rows, PaletteIcon::Glyph(IconName::Clock)),
                )],
            ),
            PaletteKind::Fork => (
                SharedString::from(""),
                SharedString::from("Pick a completed turn to branch from"),
                vec![PaletteSection::new(
                    "Fork from",
                    palette_items(&rows, PaletteIcon::Glyph(IconName::Git)),
                )],
            ),
            PaletteKind::Search => {
                let (sessions, files): (Vec<_>, Vec<_>) =
                    rows.iter().partition(|(id, _, _)| id.starts_with("s:"));
                // The row's own match emphasis covers the label only — the
                // library paints `matched` ranges on the label and the
                // context (the snippet) stays muted mono. Primary text is
                // the sidebar label either way; the snippet is display-only.
                let query = self.search_query.read(cx).value().to_string();
                let mut sections = Vec::new();
                if !sessions.is_empty() {
                    sections.push(PaletteSection::new(
                        "Sessions",
                        palette_items_ref_matching(&sessions, PaletteIcon::Glyph(IconName::Clock), &query),
                    ));
                }
                if !files.is_empty() {
                    sections.push(PaletteSection::new(
                        "Files",
                        palette_items_ref_matching(&files, PaletteIcon::Glyph(IconName::File), &query),
                    ));
                }
                (SharedString::from(""), self.search_status(cx), sections)
            }
        };
        // The click carries the row id, so it runs that row directly instead
        // of rebuilding every row to map the id back to a position and the
        // position back to the same id (finding `performance-7`).
        let select = cx.listener(move |this: &mut Self, id: &SharedString, window, cx| {
            this.run_palette_row(kind, id.clone(), window, cx);
        });
        let dismiss = cx.listener(|this: &mut Self, _: &(), _, cx| {
            this.overlays.update(cx, |overlays, _| overlays.palette = None);
            cx.notify();
        });
        let count = rows.len();
        // A scripted screenshot is a static composition, not an opening: the
        // card's enter presence (fade + rise) never settles inside a capture,
        // so screenshots draw the palette at rest — opaque, one surface.
        // Live opens keep the rise.
        let mut card = command_palette("palette", query, sections, selected)
            .placeholder(placeholder)
            .on_select(move |id, w, cx| select(id, w, cx))
            .on_dismiss(move |w, cx| dismiss(&(), w, cx));
        if self.still() {
            card = card.at_rest();
        }
        Some(
            popover_layer(
                div()
                    .key_context(aui::keys::MENU_CONTEXT)
                    .track_focus(&self.focus_palette)
                    .on_action(cx.listener(move |this, _: &SelectNext, _, cx| {
                        this.overlays.update(cx, |o, _| o.move_palette(1, count));
                        cx.notify();
                    }))
                    .on_action(cx.listener(move |this, _: &SelectPrev, _, cx| {
                        this.overlays.update(cx, |o, _| o.move_palette(-1, count));
                        cx.notify();
                    }))
                    .on_action(cx.listener(|this, _: &Confirm, window, cx| this.confirm_palette(window, cx)))
                    .on_action(cx.listener(|this, _: &Cancel, _, cx| {
                        this.overlays.update(cx, |overlays, _| overlays.palette = None);
                        cx.notify();
                    }))
                    .absolute()
                    .inset_0()
                    .bg(gpui::black().opacity(PALETTE_SCRIM))
                    // A press on the dimmed ground closes it, which is the
                    // gesture every overlay in this window already answers to.
                    .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| {
                        this.overlays.update(cx, |overlays, _| overlays.palette = None);
                        cx.notify();
                    }))
                    .child(
                        gpui_kit::base::h_flex()
                            .w_full()
                            .justify_center()
                            .pt(px(PALETTE_TOP))
                            .child(
                                v_flex()
                                    // The search field is the sidebar list's
                                    // box and carries its side margins;
                                    // centring the column lands the field's
                                    // visible box exactly on the card's, so
                                    // the two read as one surface.
                                    .items_center()
                                    .children(self.render_search_input(cx))
                                    .child(card),
                            ),
                    ),
            )
            .into_any_element(),
        )
    }

    pub(crate) fn render_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        // Read the modal out whole before anything asks `cx` for a listener:
        // the entity's borrow and `cx.listener` cannot be alive at once.
        let (title, detail, kind, primary_label, action, danger) = {
            let modal = self.overlays.read(cx).dialog.as_ref()?;
            let danger = modal.action == DialogAction::Archive;
            (modal.title.clone(), modal.detail.clone(), modal.kind, modal.primary, modal.action, danger)
        };
        // The archive target stays on the dialog until its own button runs:
        // closing it any other way drops the target with it.
        let secondary = if danger { "Cancel" } else { "Dismiss" };
        let primary = cx.listener(move |this: &mut Self, _: &(), window, cx| {
            if action == DialogAction::Archive {
                this.confirm_archive_dialog(window, cx);
                return;
            }
            this.close_dialog(cx);
            match action {
                DialogAction::Dismiss => {}
                DialogAction::Reconnect => {
                    this.wire = Wire::Reconnecting;
                    this.reconnect(cx);
                }
                DialogAction::SignIn => {
                    this.auth = Auth::SignedOut;
                    this.active = None;
                    this.login.reset_to_choose();
                }
                DialogAction::Archive => {}
            }
            cx.notify();
        });
        let close = cx.listener(|this: &mut Self, _: &(), _, cx| this.close_dialog(cx));
        // `cx.listener` hands back an opaque `Fn`, not a `Clone`, so the scrim
        // gets its own rather than sharing the secondary button's.
        let dismiss = cx.listener(|this: &mut Self, _: &(), _, cx| this.close_dialog(cx));
        // A deterministic capture draws the dialog settled rather than rising
        // in: the enter presence never lands on the same frame twice.
        let card = dialog("dialog", title)
            .kind(kind)
            .body(detail)
            .danger(danger)
            .secondary(secondary)
            .primary(primary_label)
            .on_primary(move |w, cx| primary(&(), w, cx))
            .on_secondary(move |w, cx| close(&(), w, cx))
            .on_dismiss(move |w, cx| dismiss(&(), w, cx));
        let card = if crate::clock::deterministic() { card.at_rest() } else { card };
        Some(
            popover_layer(
                div()
                    .absolute()
                    .inset_0()
                    .key_context(aui::keys::MENU_CONTEXT)
                    .track_focus(&self.focus_dialog)
                    .on_action(cx.listener(|this, _: &Cancel, _, cx| this.close_dialog(cx)))
                    .child(card),
            )
            .into_any_element(),
        )
    }

    /// Harness → About Harness.
    pub(crate) fn show_about(&mut self, cx: &mut Context<Self>) {
        self.set_dialog(
            cx,
            Dialog {
                title: "About Harness".into(),
                detail: format!(
                    "Harness {} \u{2014} a macOS chat interface to Muse Code.\n\nKeys: docs/08-keymap.md. App: docs/02-app.md.",
                    env!("CARGO_PKG_VERSION")
                ),
                kind: DialogKind::Info,
                primary: "OK",
                action: DialogAction::Dismiss,
                archive_target: None,
            },
        );
    }

    /// The toast stack, bottom right. Toasts here are informational — a
    /// compaction that did nothing, a command a later phase brings — so they
    /// carry no action, only a close.
    pub(crate) fn render_toasts(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let toasts = self.overlays.read(cx).toasts.clone();
        if toasts.is_empty() {
            return None;
        }
        let newest = toasts.last().map(|t| t.id.to_string()).unwrap_or_default();
        let dismissed = newest.clone();
        let close = cx.listener(move |this: &mut Self, _: &(), _, cx| {
            this.overlays.update(cx, |overlays, _| overlays.dismiss_toast(&dismissed));
            cx.notify();
        });
        // One action exists, and it is Undo — one hidden row, one "Clear
        // empty" batch, or one archived session, whichever the newest toast
        // was for.
        let act = cx.listener(move |this: &mut Self, _: &(), _, cx| {
            this.undo_newest(cx);
            this.overlays.update(cx, |overlays, _| overlays.dismiss_toast(&newest));
            cx.notify();
        });
        // A deterministic capture draws the stack settled: toasts slide in,
        // which never lands on the same frame twice.
        let stack = aui::feedback::toast_stack("toasts", toasts);
        let stack = if crate::clock::deterministic() { stack.at_rest() } else { stack };
        Some(
            popover_layer(
                div()
                    .absolute()
                    .right(px(scale::SP_5))
                    .top(px(TOAST_TOP))
                    .w(px(TOAST_W))
                    .h(px(TOAST_STACK_H))
                    .child(
                        stack
                            .on_action(move |_, window, cx| act(&(), window, cx))
                            .on_close(move |window, cx| close(&(), window, cx)),
                    ),
            )
            .into_any_element(),
        )
    }
}
