//! The sidebar column: what it draws, and the two menus that hang off it.
//!
//! Everything here is composition over state [`Harness`] already holds — the
//! joined session rows, the filter toggles, the open rename, the signed-in
//! identity and the probed tier. It is the shell's left half plus the
//! session-list policy that decides which rows are shown and what the column
//! says when it shows none; it never touches login, the wire, or the session
//! lifecycle, and the only thing it writes is a filter toggle.
//!
//! [`crate::sidebar`] next door is the pure half: rows in, a [`Grouping`] out.
//! This file is the half that needs a `Window`.
//!
//! Two popovers belong to the column rather than to the overlay stack, because
//! each is anchored to the affordance that opens it (the design rule in
//! [`crate::overlays`]): the Sessions caption's view menu, where list
//! management lives, and the footer's account menu, where Sign out lives.
//!
//! [`Grouping`]: aui::nav::Grouping

use aui::data::{icon_button, ButtonSize};
use aui::nav::{dense_field, nav_item, rail, sidebar_footer, sidebar_view, view_menu, MenuRow, RailItem, RowAction};
use aui::overlay::popover_layer;
use aui_icons::{IconName, Provider};
use aui_tokens::{scale, ActiveAui, AgentState, AuiStyled};
use gpui::{
    div, prelude::*, px, AnyElement, Context, Focusable, SharedString, Window,
};
use gpui_kit::base::v_flex;
use muse_client::schema::AccountStateKind;

use crate::app::{ConfirmRename, Harness, RENAME_CONTEXT};
use crate::login::Auth;
use crate::overlays::MenuKind;
use crate::sidebar::SessionEntry;

/// Actions of the Sessions caption's view menu, in row order.
#[derive(Clone, Copy)]
enum ViewAction {
    ToggleEmpty,
    ToggleHidden,
    ClearEmpty,
    ToggleArchived,
}

impl Harness {
    /// The two rows above the Sessions caption: New session, and Automations
    /// behind a Soon tag until it has somewhere to go.
    fn render_nav_block(&self, cx: &mut Context<Self>) -> AnyElement {
        let new_session = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.new_session(cx));
        let automations = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
            this.overlays.update(cx, |overlays, _| {
                overlays.toast("Automations", "Automations are not wired up yet.");
            });
            cx.notify();
        });
        v_flex()
            .w_full()
            .flex_none()
            .px(px(scale::SP_2))
            .pt(px(scale::SP_2))
            .child(nav_item("nav-new", IconName::Plus, "New session").on_click(new_session))
            .child(nav_item("nav-automations", IconName::Zap, "Automations").count("Soon").on_click(automations))
            .into_any_element()
    }

    pub(crate) fn render_sidebar(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let visible = self.visible_sessions(cx);
        let empty = self.render_sidebar_empty(&visible, cx);
        // Both come from the window's cache: built once per change and once
        // per minute, not once per frame (findings `performance-5`,
        // `support-2`). The library takes the grouping behind an `Rc`
        // (finding `performance-13`), so the frame hands the cached one
        // straight over and clones nothing.
        let grouping = self.sidebar_grouping(cx);
        // The click's target first: the row highlights on the click's own
        // frame, before the new view (or any page) exists.
        let selected =
            self.pending_id.clone().or_else(|| self.active.as_ref().map(|a| a.read(cx).session_id.clone()));
        let select = cx.listener(|this: &mut Self, id: &SharedString, window, cx| {
            this.resume(id.to_string(), window, cx);
        });
        let act = cx.listener(|this: &mut Self, (id, action): &(SharedString, RowAction), window, cx| {
            match action {
                RowAction::Rename => this.start_rename(id.to_string(), window, cx),
                RowAction::Pin => this.toggle_pin(id.to_string(), cx),
                // The tray carries one archive affordance: on a listed session
                // it asks first, on an archived one it puts it straight back.
                RowAction::Archive => {
                    let archived =
                        this.sessions.iter().find(|e| e.id == id.as_ref()).is_some_and(|e| e.archived);
                    if archived {
                        this.unarchive_session(id.to_string(), cx);
                    } else {
                        this.open_archive_dialog(id.to_string(), cx);
                    }
                }
                _ => {}
            }
        });
        // The sliders icon toggles the view menu like every other popover.
        let open_view = cx.listener(|this: &mut Self, _: &(), _, cx| {
            this.open_menu(MenuKind::ViewOptions, cx);
        });
        let mut view = sidebar_view("sessions", grouping)
            .caption("Sessions")
            .on_view_options(move |w, cx| open_view(&(), w, cx))
            .row_actions(vec![RowAction::Pin, RowAction::Rename, RowAction::Archive])
            .on_select(move |id, w, cx| select(id, w, cx))
            .on_action(move |id, action, w, cx| act(&(id.clone(), action), w, cx));
        if let Some(renaming) = self.renaming.clone() {
            view = view.editing(renaming, self.rename_field(window, cx));
        }
        if let Some(selected) = selected {
            view = view.selected(selected);
        }
        // No quick-filter field: ⌘⇧F and the sidebar search icon open the
        // full-text search palette instead, so the two can never share the
        // sidebar.
        v_flex()
            .size_full()
            .child(self.render_nav_block(cx))
            .child(
                div()
                    .id("sessions-scroll")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    // Tracked so the Sessions view menu can anchor under the
                    // caption's sliders icon (the caption scrolls with this
                    // list); observing changes nothing about the scrolling
                    // itself.
                    .track_scroll(&self.sessions_scroll)
                    .child(view)
                    .children(empty),
            )
            .child(self.render_footer(cx))
            .into_any_element()
    }

    /// The field the row being renamed holds: the library's dense recipe —
    /// a borderless, chromeless single line at the row-title size, with the
    /// 1 px focus border on the wrapper instead of the component. The wrapper
    /// is a flex row centring its child, fixed to the 22 px box the module
    /// docs in `aui::nav::session_row` prescribe (20 px of field in 4 px of
    /// row padding is exactly the 30 px row): the editor's own line box plus
    /// its internal padding used to stand taller, so the row grew and the
    /// list below jumped. Clipping is horizontal only, so a long name scrolls
    /// under the caret instead of spilling a second line, and the row keeps
    /// its own height while a rename is open, so siblings never move. The
    /// commit path is unchanged. The same element serves the sidebar row and
    /// the header title.
    pub(crate) fn rename_field(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let focused = self.rename.focus_handle(cx).is_focused(window);
        div()
            .w_full()
            .key_context(RENAME_CONTEXT)
            .on_action(cx.listener(|this, _: &ConfirmRename, window, cx| this.commit_rename(window, cx)))
            .child(
                div()
                    .w_full()
                    .h(px(22.0))
                    .flex()
                    .items_center()
                    .overflow_x_hidden()
                    .py(px(0.0))
                    .px(px(6.0))
                    .rounded(px(scale::R_SM))
                    .border_1()
                    .border_color(if focused { p.accent } else { p.line })
                    .bg(p.surface_1)
                    // The editor keeps its natural line box (`h_auto`): the
                    // library's 20 px recipe stands its glyphs on the bottom
                    // edge of a 22 px wrapper and clips their descenders. The
                    // wrapper is the fixed box that keeps the row's height;
                    // the editor centres in it and may overhang the border by
                    // a pixel or two of line box, which draws nothing.
                    .child(dense_field(&self.rename).h_auto().whitespace_nowrap().overflow_x_hidden()),
            )
            .into_any_element()
    }

    /// What the list says when it has nothing to show, and why (spec §5).
    fn render_sidebar_empty(&self, visible: &[SessionEntry], cx: &mut Context<Self>) -> Option<AnyElement> {
        if !visible.is_empty() {
            return None;
        }
        let p = cx.aui().colors;
        let active = self.active_id(cx);
        // What each filter alone is keeping out, past the other one: the
        // empty text names its own toggle rather than borrowing hidden's.
        // (There is no sidebar text filter — search lives in the palette —
        // so no "no match" state exists here.)
        let hidden_only = !self.show_hidden && self.sessions.iter().any(|e| e.hidden);
        let empty_only = !self.show_empty
            && self
                .sessions
                .iter()
                .any(|e| (self.show_hidden || !e.hidden) && e.is_empty(active.as_deref()));
        let (title, detail) = match (hidden_only, empty_only) {
            (true, _) => {
                ("Every session here is hidden", "Turn on \u{201c}Show hidden\u{201d} in the Sessions menu above.")
            }
            (false, true) => {
                ("Only empty sessions here", "Turn on \u{201c}Show empty\u{201d} in the Sessions menu above.")
            }
            (false, false) => ("No sessions yet", "\u{2318}N starts one."),
        };
        Some(
            v_flex()
                .w_full()
                .px(px(scale::SP_5))
                .py(px(scale::SP_6))
                .gap(px(scale::SP_2))
                .child(div().ui(scale::FS_12).medium().text_color(p.ink_2).child(title))
                .child(div().ui(scale::FS_11).text_color(p.ink_4).child(detail))
                .into_any_element(),
        )
    }

    /// "Signed in as", in the library's shape: avatar, name, the email it is
    /// really reporting, the plan row, and the provider usage meter with the
    /// chevron. The whole footer opens the account menu — Sign out lives
    /// there now, and the list-management toggles live in the Sessions menu.
    fn render_footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let Auth::SignedIn(identity) = &self.auth else {
            return div().into_any_element();
        };
        let account = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
            this.open_menu(MenuKind::Account, cx);
        });
        let mut footer = sidebar_footer("account", identity.initial(), identity.footer_name())
            .on_click(move |e, w, cx| account(e, w, cx));
        if !identity.email.is_empty() {
            footer = footer.detail(identity.email.clone());
        }
        // The third row: what this login is entitled to. Warning-tinted for
        // anything that is not a plan in force, because that is the case where
        // the next turn costs money nobody expected. The key lanes say so
        // without a probe: a stored key or `META_API_KEY` is pay-as-you-go by
        // construction.
        let meter = self.tier.as_ref().and_then(|tier| tier.weekly_fraction());
        // `--tier` fakes the probe it names: the footer reads the faked tier
        // like any other probe answer, even on the key lanes.
        if self.args.tier.is_some() {
            if let Some(tier) = &self.tier {
                footer = footer.plan(tier.footer_label(), tier.is_warning());
            }
        } else if identity.is_api_key() {
            footer = footer.plan("Pay-as-you-go · API key", true);
        } else if let Some(tier) = &self.tier {
            footer = footer.plan(tier.footer_label(), tier.is_warning());
        }
        // The meter is the weekly fraction the probe already reports; with no
        // reading there is no meter. Either way the chevron stands, so the
        // account menu stays discoverable — the row's own click opens it too.
        if let Some(fraction) = meter {
            footer = footer.meter(Provider::Muse, fraction);
        } else {
            footer = footer.trailing(
                icon_button("account-chevron", IconName::ChevronDown)
                    .ghost()
                    .size(ButtonSize::Xs)
                    .icon_size(px(12.0)),
            );
        }
        footer.into_any_element()
    }

    /// The collapsed rail: new-session and search cells, a separator, one dot
    /// per running session mirroring the rows, and the account avatar.
    pub(crate) fn render_rail(&self, cx: &mut Context<Self>) -> AnyElement {
        let active = self.active_id(cx);
        let mut items = vec![
            RailItem::nav("new", IconName::Plus),
            RailItem::nav("search", IconName::Search),
            RailItem::separator(),
        ];
        for entry in &self.sessions {
            if entry.hidden || entry.archived || !entry.running {
                continue;
            }
            let mut cell = RailItem::session(entry.id.clone(), AgentState::Running).pulse();
            if active.as_deref() == Some(entry.id.as_str()) {
                cell = cell.selected(true);
            }
            items.push(cell);
        }
        let mut rail = rail("rail", items).flat(true);
        if let Auth::SignedIn(identity) = &self.auth {
            rail = rail.avatar(identity.initial());
        }
        let select = cx.listener(|this: &mut Self, id: &SharedString, window, cx| {
            this.resume(id.to_string(), window, cx);
        });
        let action = cx.listener(|this: &mut Self, name: &str, window, cx| match name {
            "new" => this.new_session(cx),
            // Task E has landed: the rail cell opens the full-text search
            // palette, like the header search icon and ⌘⇧F.
            "search" => this.open_search(window, cx),
            "account" => this.open_menu(MenuKind::Account, cx),
            _ => {}
        });
        rail
            .on_select(move |id, w, cx| select(id, w, cx))
            .on_action(move |name, w, cx| action(name, w, cx))
            .into_any_element()
    }

    /// The Sessions caption's view menu: where list management lives now that
    /// the footer is the library's account row again.
    ///
    /// Anchored under the caption's sliders icon, right edge aligned to the
    /// sidebar's content edge. The caption row is the scroll content's first
    /// child — an 8 px top margin (`scale::SP_3`, the library row's default)
    /// and 28 px tall, the icon at its right end under 12 px of row padding
    /// (both read out of `aui::nav::parts`, which keeps them private) — plus
    /// a 4 px gap under it. The tracked scroll handle reports the viewport
    /// and its offset in window coordinates every prepaint, so the menu
    /// follows the sidebar's width and the list's scroll; before the first
    /// prepaint it keeps the old fixed seat.
    pub(crate) fn render_view_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.overlays.read(cx).is_open(MenuKind::ViewOptions) {
            return None;
        }
        let active = self.active_id(cx);
        let hidden = self.sessions.iter().filter(|e| e.hidden).count();
        let empty = self.sessions.iter().filter(|e| !e.archived && e.is_empty(active.as_deref())).count();
        let archived = self.sessions.iter().filter(|e| e.archived).count();
        let mut rows: Vec<MenuRow> = Vec::new();
        let mut actions: Vec<Option<ViewAction>> = Vec::new();
        // Either toggle only appears once it has something to show: an
        // affordance for an empty set is a question nobody asked.
        if empty > 0 || self.show_empty {
            let label = if self.show_empty {
                "Hide empty".to_owned()
            } else {
                format!("Show empty ({empty})")
            };
            rows.push(MenuRow::Toggle { label: label.into(), checked: self.show_empty });
            actions.push(Some(ViewAction::ToggleEmpty));
        }
        if hidden > 0 || self.show_hidden {
            let label = if self.show_hidden {
                "Hide hidden".to_owned()
            } else {
                format!("Show hidden ({hidden})")
            };
            rows.push(MenuRow::Toggle { label: label.into(), checked: self.show_hidden });
            actions.push(Some(ViewAction::ToggleHidden));
        }
        if empty > 0 {
            rows.push(MenuRow::Toggle { label: "Clear empty".into(), checked: false });
            actions.push(Some(ViewAction::ClearEmpty));
        }
        if !rows.is_empty() {
            rows.push(MenuRow::Separator);
            actions.push(None);
        }
        let archived_label = if self.show_archived {
            "Hide archived".to_owned()
        } else {
            format!("Show archived ({archived})")
        };
        rows.push(MenuRow::Toggle { label: archived_label.into(), checked: self.show_archived });
        actions.push(Some(ViewAction::ToggleArchived));
        let activate = cx.listener(move |this: &mut Self, index: &usize, _, cx| {
            match actions.get(*index).copied().flatten() {
                // Toggles keep the menu open, so the check is seen to change.
                Some(ViewAction::ToggleEmpty) => {
                    this.show_empty = !this.show_empty;
                    cx.notify();
                }
                Some(ViewAction::ToggleHidden) => {
                    this.show_hidden = !this.show_hidden;
                    cx.notify();
                }
                Some(ViewAction::ToggleArchived) => {
                    this.show_archived = !this.show_archived;
                    cx.notify();
                }
                Some(ViewAction::ClearEmpty) => {
                    this.overlays.update(cx, |overlays, _| overlays.menu = None);
                    this.clear_empty(cx);
                }
                None => {}
            }
        });
        // Caption geometry the library keeps private (`aui::nav::parts`):
        // 28 px row, 12 px horizontal padding, 4 px gap under the menu. The
        // menu itself is a fixed 250 px (`aui::nav::view_menu`, private
        // `MENU_W`): right-align it to the caption's right edge, clamped
        // into the window so a sidebar narrower than the menu never clips
        // its left side.
        const CAPTION_H: f32 = 28.0;
        const CAPTION_PAD_X: f32 = 12.0;
        const MENU_GAP: f32 = 4.0;
        const MENU_W: f32 = 250.0;
        let viewport = self.sessions_scroll.bounds();
        let placed: Option<(f32, f32)> = if f32::from(viewport.size.width) > 0.0 {
            // The view is the scroll content's first child; the caption sits
            // at its head under the row's top margin.
            let content_top = f32::from(
                self.sessions_scroll
                    .bounds_for_item(0)
                    .map(|bounds| bounds.origin.y)
                    .unwrap_or(viewport.origin.y),
            );
            let top = (content_top - f32::from(self.sessions_scroll.offset().y) + scale::SP_3 + CAPTION_H
                + MENU_GAP)
                .max(f32::from(viewport.origin.y) + MENU_GAP);
            let right_edge = f32::from(viewport.origin.x) + f32::from(viewport.size.width) - CAPTION_PAD_X;
            let left = (right_edge - MENU_W).max(8.0);
            Some((top, left))
        } else {
            None
        };
        let mut pop = div().absolute();
        pop = match placed {
            Some((top, left)) => pop.top(px(top)).left(px(left)),
            None => pop.top(px(140.0)).left(px(12.0)),
        };
        Some(
            popover_layer(
                pop.child(view_menu("sessions-view", rows).at_rest().on_activate(move |i, w, cx| activate(&i, w, cx))),
            )
            .into_any_element(),
        )
    }

    /// The footer's account menu: Sign out, and nothing else. The environment
    /// lane names itself: `META_API_KEY` survives a sign-out, so the row says
    /// where the credential really comes from (D28).
    pub(crate) fn render_account_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.overlays.read(cx).is_open(MenuKind::Account) {
            return None;
        }
        let label = match &self.auth {
            Auth::SignedIn(identity) if identity.lane == AccountStateKind::EnvKey => {
                "Sign out (set by META_API_KEY)"
            }
            _ => "Sign out",
        };
        let rows = vec![MenuRow::Toggle { label: label.into(), checked: false }];
        let activate = cx.listener(move |this: &mut Self, index: &usize, _, cx| {
            if *index == 0 {
                this.overlays.update(cx, |overlays, _| overlays.menu = None);
                this.logout(cx);
            }
        });
        Some(
            popover_layer(
                div()
                    .absolute()
                    .bottom(px(100.0))
                    .left(px(12.0))
                    .child(view_menu("account", rows).at_rest().on_activate(move |i, w, cx| activate(&i, w, cx))),
            )
            .into_any_element(),
        )
    }
}
