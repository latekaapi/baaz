//! The root's half of the sidebar (task 4.7): what only the root knows,
//! pushed into the column as [`SidebarFacts`]; the column's intents, mapped
//! to the handlers that act on them; and the hover card the root seats
//! beside the column.
//!
//! The column itself — its rows, scroll, hover, reveal, wheel and fade —
//! is [`SidebarPane`] in [`crate::sidebar_view`], a view of its own over the
//! [`crate::session_list::SessionList`] model. Nothing here draws the
//! column; the root pushes its facts while it composes the shell.
//!
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;

use aui::browse::split_address;
use aui::composer::provider_display_name;
use aui::data::{icon_button, ButtonSize};
use aui::nav::{anchored_session_detail_at_sidebar, dense_field, GroupAction, RowAction};
use aui::screens::sidebar::{
    Account, AccountAction, BrowseEditing, BrowseFolder, BrowseIntent, BrowseModel, BrowseNaming, BrowsePin,
    BrowseSpace, BrowseTab, FolderKind, GroupKey, ProviderUsage, SidebarIntent, SortKey, TaskFolderAction, UsageLine,
    ViewOption,
};
use aui::screens::UsageRowState;
use aui_tokens::Palette;
use gpui::{ElementId, ListOffset, WeakEntity};

use crate::sidebar_view::{
    browse_tab_id, browse_tab_key, footer_identity, tier_plan_label, task_face_for, SidebarEvent, SidebarFacts,
    SidebarPane, NAV_NEEDS_YOU, NAV_PROJECTS, NAV_SKILLS,
};
use crate::sidebar::SessionRow as _;

/// One person tab the Browse sidebar draws, with the live page facts
/// behind its row state.
pub(crate) struct BrowseTabSnap {
    pub(crate) id: crate::browser::TabId,
    pub(crate) title: String,
    pub(crate) initial: String,
    pub(crate) active: bool,
    pub(crate) pinned: bool,
    pub(crate) loading: bool,
    /// The favicon's local file, once fetched ([`crate::browser::favicons`]).
    pub(crate) favicon: Option<String>,
}

/// One session tab under its task folder, still owned by its session.
pub(crate) struct BrowseSessionTabSnap {
    favicon: Option<String>,
    id: crate::browser::TabId,
    title: String,
    initial: String,
    active: bool,
}

impl Harness {
    /// What only the root knows and the column draws this frame
    /// ([`SidebarFacts`]): the list's root facts, the selection, the
    /// current project, the footer, the Skills count, the menus standing
    /// open, the inline rename, Browse mode's rows and the grouping minute.
    pub(crate) fn sidebar_facts(&self, cx: &mut Context<Self>) -> SidebarFacts {
        let list = self.list_facts(cx);
        // One identity per chain: the selected row is the head (Y2a).
        let selected = list.pending.clone().or_else(|| list.active.clone()).map(|id| self.chain_head(&id, cx));
        let account_menu_open = self.overlays.read(cx).is_open(MenuKind::Account);
        let browse = (crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse).then(|| {
            // The naming draft stays out of the facts: the field draws
            // itself, and the pane lays the live draft over the model.
            let mut browse = self.browse_model(cx);
            if let Some(editing) = browse.editing.as_mut() {
                editing.draft = SharedString::default();
            }
            browse
        });
        let minute = self.session_list.read(cx).grouping_minute(&list, cx);
        SidebarFacts {
            selected,
            current_project: self.current_project.clone(),
            account: self.sidebar_account(account_menu_open, cx),
            skills_on: self.skills_page.read(cx).catalog.on_count(),
            skills_offered: crate::skills_page::skills_offered(cx),
            view_menu_open: self.overlays.read(cx).is_open(MenuKind::ViewOptions),
            row_menu: self.open_row_menu(cx),
            row_menu_offers: self.open_row_menu(cx).map(|id| self.row_menu_offers(&id, cx)).unwrap_or_default(),
            row_menu_at: self.overlays.read(cx).menu().filter(|menu| menu.kind == MenuKind::SessionRow).and_then(|menu| menu.point),
            browse_menu: self.browse_menu_now(cx),
            renaming: self.renaming(cx),
            browse,
            minute,
            list,
        }
    }

    /// Push this frame's facts into the column ([`SidebarPane::push_facts`]),
    /// answering whether they moved. Never notifies the root.
    pub(crate) fn push_sidebar_facts(&mut self, cx: &mut Context<Self>) -> bool {
        let facts = self.sidebar_facts(cx);
        self.sidebar_pane.update(cx, |pane, cx| pane.push_facts(facts, cx))
    }

    /// The column's facts, from the root's render while it composes the
    /// shell. A notify raised inside a draw lands on the next frame, and a
    /// quiet window may take no frame for seconds: when the facts moved,
    /// ask for the next one so the column draws them now.
    pub(crate) fn compose_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.push_sidebar_facts(cx) {
            crate::log::boot_mark("pane-notify");
            aui_motion::request_frame(window);
        }
    }

    /// What the column asks for ([`SidebarEvent`]).
    pub(crate) fn on_sidebar_event(
        &mut self,
        _: &Entity<SidebarPane>,
        event: &SidebarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SidebarEvent::Intent(intent) => self.sidebar_intent(intent.clone(), window, cx),
            SidebarEvent::Scrolled => self.dismiss_group_menu(cx),
            // The root seats the hover card, and the frame trace takes its
            // row per root render.
            SidebarEvent::DetailChanged | SidebarEvent::SweepTick => cx.notify(),
        }
    }

    /// The sessions list's scroll position ([`SidebarPane::list_top`]).
    pub(crate) fn sidebar_list_top(&self, cx: &gpui::App) -> ListOffset {
        self.sidebar_pane.read(cx).list_top()
    }

    /// Whether a sidebar wheel gesture is in flight ([`SidebarPane::gesture_active`]).
    pub(crate) fn sidebar_gesture_active(&self, cx: &gpui::App) -> bool {
        self.sidebar_pane.read(cx).gesture_active()
    }


    /// The local favicon file for `tab`, once its icon has been fetched.
    fn favicon_file(&self, tab: &crate::browser::tabs::Tab, cx: &gpui::App) -> Option<String> {
        let url = tab.favicon.as_deref()?;
        Some(self.browser.read(cx).favicons.file(url)?.to_string_lossy().into_owned())
    }

    pub(crate) fn browse_person_snaps(&self, cx: &gpui::App) -> Vec<BrowseTabSnap> {
        if !self.browser.read(cx).tabs_loaded {
            return Vec::new();
        }
        let space = self.browser.read(cx).browse_space();
        let active =
            self.browser.read(cx).tabs.active_of(&crate::browser::Owner::Person, space.as_deref());
        self.browser
            .read(cx)
            .tabs
            .person_tabs(space.as_deref())
            .iter()
            .map(|tab| {
                let page = self.browser.read(cx).states.get(&tab.id);
                let loading = match page {
                    Some(state) => {
                        let progress = state.read(cx).progress();
                        progress > 0.0 && progress < 1.0
                    }
                    None => false,
                };
                BrowseTabSnap {
                    id: tab.id,
                    title: crate::browser::tabs::tab_label(&tab.title, &tab.url),
                    initial: crate::browser::tabs::tab_initial(&tab.title, &tab.url),
                    active: Some(tab.id) == active,
                    pinned: tab.pinned,
                    loading,
                    favicon: self.favicon_file(tab, cx),
                }
            })
            .collect()
    }

    /// Every session in the current project that owns tabs, in
    /// first-seen order: the session id, its sidebar name (or "New
    /// session" when it has no row yet), and its tabs.
    fn browse_session_groups(&self, cx: &gpui::App) -> Vec<(String, String, Vec<BrowseSessionTabSnap>)> {
        if !self.browser.read(cx).tabs_loaded {
            return Vec::new();
        }
        let visible = self.visible_sessions(cx);
        self.browser
            .read(cx)
            .tabs
            .session_tabs(None)
            .iter()
            .map(|(session, tabs)| {
                let owner = crate::browser::Owner::Session(session.clone());
                let active = self.browser.read(cx).tabs.active_of(&owner, None);
                // The sidebar name, or "New session" while the session
                // has no row yet (an unsent session is filtered from the
                // list) — never the bare "Session".
                let label = visible
                    .iter()
                    .find(|entry| entry.id == *session)
                    .map(|entry| entry.label.clone())
                    // A browser chat is hidden from the list: its folder
                    // takes the card's title (R3).
                    .or_else(|| self.is_browser_chat(session, cx).then(|| self.browser_chat_title_for(session, cx)))
                    .unwrap_or_else(|| crate::sidebar::UNNAMED.to_owned());
                let rows = tabs
                    .iter()
                    .map(|tab| BrowseSessionTabSnap {
                        favicon: self.favicon_file(tab, cx),
                        id: tab.id,
                        title: crate::browser::tabs::tab_label(&tab.title, &tab.url),
                        initial: crate::browser::tabs::tab_initial(&tab.title, &tab.url),
                        active: Some(tab.id) == active,
                    })
                    .collect();
                (session.clone(), label, rows)
            })
            .collect()
    }

    /// The sidebar's toggle, drawn by the shell in the sidebar's chrome row
    /// (open) and in the peek card's row (closed).
    pub(crate) fn render_sidebar_toggle(&self, cx: &mut Context<Self>) -> AnyElement {
        let show = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.toggle_sidebar(cx));
        // One label in both states: a state-specific one is captured when the
        // tooltip arms and then reads wrong for a moment after the click flips
        // the state (seen live 2026-10-13: "Show sidebar" over an open sidebar).
        let label = "Toggle sidebar  ⌘B";
        div()
            .id("hd-minimal-sidebar")
            .debug_selector(|| "hd-minimal-sidebar".into())
            .flex_none()
            .child(
                icon_button("sidebar-show", IconName::Sidebar)
                    .ghost()
                    .size(ButtonSize::Sm)
                    .accessibility_label(label)
                    .on_click(move |e, w, cx| show(e, w, cx)),
            )
            .into_any_element()
    }

    /// The footer and its menu: the signed-in person (or the signed-out
    /// placeholder), the tier's plan label, and — only while the menu is
    /// open — one usage card per enabled provider from `usage_rows`.
    fn sidebar_account(&self, menu_open: bool, cx: &gpui::App) -> Account {
        // Every provider alike (owner, 2026-10-29: the footer read Muse's
        // sign-in as the app's): the account of the provider new sessions
        // start on when it is connected, else the first connected one in
        // the one provider order. A billing-tier provider's plan label
        // comes from its tier probe.
        let tier_plan = || tier_plan_label(self.tier.as_ref()).unwrap_or_default();
        let (name, email, initial, plan, status) = footer_identity(self.provider_statuses(cx), self.new_provider(cx), tier_plan);
        let usage = if menu_open { self.sidebar_usage() } else { Vec::new() };
        Account {
            name: name.into(),
            email: email.into(),
            plan: plan.into(),
            initial: initial.into(),
            status,
            menu_open,
            usage,
        }
    }

    /// The account menu's usage cards: `usage_rows` (the Providers page's
    /// rows) re-shaped as the screen's `ProviderUsage` — one meter line per
    /// window, or one unavailable line carrying the reason.
    fn sidebar_usage(&self) -> Vec<ProviderUsage> {
        let signed_in = matches!(self.auth, Auth::SignedIn(_));
        let now = crate::account_usage::now_secs();
        let muse_feed = signed_in.then(|| crate::account_usage::TierFeed { tier: self.tier.clone() });
        crate::account_usage::usage_rows(&crate::provider_status::live_statuses(), muse_feed, now)
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                let lines = match row.state {
                    UsageRowState::Windows(windows, _) => windows
                        .into_iter()
                        .map(|w| UsageLine { label: w.label, fraction: w.used_fraction, resets: w.resets_at_text })
                        .collect(),
                    UsageRowState::Unavailable(reason) => {
                        vec![UsageLine { label: reason, fraction: 0.0, resets: SharedString::default() }]
                    }
                };
                ProviderUsage {
                    name: provider_display_name(row.provider),
                    plan: row.plan.unwrap_or_default(),
                    initial: row.provider.letter().into(),
                    tint: index as u8,
                    lines,
                }
            })
            .collect()
    }

    /// Browse mode's rows: the current space's person tabs (pinned ones as
    /// tiles, the rest in Today or in your folders), every session that owns
    /// tabs as a task folder, the shown page's address and history, the
    /// spaces, and the open menu / name field.
    pub(crate) fn browse_model(&self, cx: &mut Context<Self>) -> BrowseModel {
        let space = self.browser.read(cx).browse_space().unwrap_or_default();
        let snaps = self.browse_person_snaps(cx);
        let folder_of: std::collections::HashMap<crate::browser::TabId, Option<String>> =
            self.browser.read(cx).tabs.tabs.iter().map(|tab| (tab.id, tab.folder.clone())).collect();
        // One tab is active in Browse: the one the centre shows. Each session
        // also has an active tab of its own, and counting those lit every
        // agent tab's row and moved the selected pill onto the last of them
        // (owner screenshots 2026-10-11: the page's own row had none).
        let shown = self.browser.read(cx).browse_centre_shown(cx);
        let tab = |id: crate::browser::TabId, title: String, initial: String, _owner_active: bool, loading: bool, favicon: Option<String>| BrowseTab {
            id: browse_tab_key(id).into(),
            title: title.into(),
            initial: initial.into(),
            active: Some(id) == shown,
            loading,
            favicon: favicon.map(Into::into),
        };
        let mut pins = Vec::new();
        let mut today = Vec::new();
        let mut in_folders: std::collections::HashMap<String, Vec<BrowseTab>> = std::collections::HashMap::new();
        for snap in snaps {
            if snap.pinned {
                pins.push(BrowsePin { id: browse_tab_key(snap.id).into(), initial: snap.initial.into(), label: snap.title.into(), favicon: snap.favicon.map(Into::into) });
                continue;
            }
            let folder = folder_of.get(&snap.id).cloned().flatten();
            let row = tab(snap.id, snap.title, snap.initial, snap.active, snap.loading, snap.favicon);
            match folder.filter(|id| self.browser.read(cx).tabs.folders.iter().any(|f| &f.id == id)) {
                Some(folder) => in_folders.entry(folder).or_default().push(row),
                None => today.push(row),
            }
        }
        let folders = self
            .browser
            .read(cx)
            .tabs
            .folders_in(&space)
            .into_iter()
            .map(|folder| BrowseFolder {
                id: folder.id.clone().into(),
                name: folder.name.clone().into(),
                kind: FolderKind::Yours,
                open: folder.open,
                tabs: in_folders.remove(&folder.id).unwrap_or_default(),
            })
            .collect();
        let task_folders = self
            .browse_session_groups(cx)
            .into_iter()
            .map(|(session, label, rows)| {
                let entry = self.session_list.read(cx).sessions.iter().find(|entry| entry.id == session);
                let page_work = self.terminal_service.agent_activity(&session);
                let asking = page_work.as_ref().is_some_and(|work| work.asking.is_some());
                let running = entry.is_some_and(|entry| entry.running) || page_work.is_some();
                let waiting = asking || entry.is_some_and(|entry| !entry.attention.is_empty() || entry.approval_command.is_some() || entry.pending_question.is_some());
                let held = page_work.as_ref().is_some_and(|work| work.held);
                let typing = page_work.as_ref().is_some_and(|work| crate::browser::face_types(&work.label));
                let (state, face) = task_face_for(waiting, held, running, typing);
                BrowseFolder {
                    id: session.into(),
                    name: label.into(),
                    kind: FolderKind::Task { state, face, status: face.status_word().into(), needs_you: waiting },
                    open: true,
                    // A session's tabs load in its own pane; their rows keep
                    // the favicon (the groups key carries no page facts).
                    tabs: rows.into_iter().map(|row| tab(row.id, row.title, row.initial, row.active, false, row.favicon)).collect(),
                }
            })
            .collect();
        let shown = self.browser.read(cx).browse_centre_shown(cx);
        // The live page's URL, falling back to the tab's — the same read
        // the toolbar makes, so the two never disagree.
        let live_url = self.browse_centre_state(cx).map(|state| state.read(cx).url().to_string()).unwrap_or_default();
        let url = if live_url.is_empty() {
            shown.and_then(|id| self.browser.read(cx).tabs.get(id)).map(|tab| tab.url.clone()).unwrap_or_default()
        } else {
            live_url
        };
        let address = split_address(&url);
        // The engine's own history says whether back and forward go anywhere.
        let (can_go_back, can_go_forward) = self
            .browse_centre_state(cx)
            .map(|state| (state.read(cx).can_go_back(), state.read(cx).can_go_forward()))
            .unwrap_or_default();
        let spaces: Vec<BrowseSpace> = self
            .browser
            .read(cx)
            .tabs
            .spaces
            .iter()
            .map(|space| BrowseSpace { id: space.id.clone().into(), name: space.name.clone().into(), own_signins: space.own_signins })
            .collect();
        let current = self.browser.read(cx).tabs.spaces.iter().position(|s| s.id == space).unwrap_or(0);
        let editing = self.browser.read(cx).browse_naming.clone().map(|target| BrowseEditing { target, draft: self.rename.read(cx).value().to_string().into() });
        // D4: the downloads button exists while downloads list; its dot
        // says one is running.
        let downloads = self.browser.read(cx).browse_downloads(cx);
        let downloads = (!downloads.is_empty())
            .then(|| downloads.iter().any(|(_, _, d)| d.state == aui_webview::DownloadStatus::Downloading));
        // docs/82 8.4: what waits on the person and the session they left.
        let (needs_you, last_session) = self.browse_context(cx);
        BrowseModel {
            bookmarked: self.browser.read(cx).browse_centre_shown(cx)
                .and_then(|id| self.browser.read(cx).tabs.get(id).map(|tab| tab.url.clone()))
                .is_some_and(|url| self.browser.read(cx).data.read(cx).is_bookmarked(&url)),
            host: address.host,
            path: address.path,
            can_go_back,
            can_go_forward,
            pins,
            tabs: today,
            folders,
            task_folders,
            spaces: if spaces.is_empty() { vec![BrowseSpace { id: crate::browser::tabs::PERSONAL.into(), name: "Personal".into(), own_signins: false }] } else { spaces },
            downloads,
            needs_you,
            last_session,
            space: current,
            // The column owns its header hover; it lays it over this.
            hovered_today_header: false,
            editing,
            menu: self.browse_menu_now(cx),
        }
    }

    /// Every intent the screen reports, routed to the handler Baaz already
    /// had for it. Any intent that is not the menus' own closes an open
    /// view-options or account menu: the screen draws the menus, the host
    /// decides when they go (outside clicks and Escape are its to detect).
    pub(crate) fn sidebar_intent(&mut self, intent: SidebarIntent, window: &mut Window, cx: &mut Context<Self>) {
        let keeps_menu = matches!(
            intent,
            SidebarIntent::ViewOptions
                | SidebarIntent::ViewOption(_)
                | SidebarIntent::Account
                | SidebarIntent::AccountAction(_)
                | SidebarIntent::RowAction(_, RowAction::More)
                | SidebarIntent::MenuBackdrop
                | SidebarIntent::DismissMenus
                | SidebarIntent::Hover(..)
                | SidebarIntent::HoverGroup(_)
                | SidebarIntent::HoverProjectsHeader(_)
                | SidebarIntent::Browse(BrowseIntent::HoverTodayHeader(_))
        );
        if !keeps_menu {
            let open = self.overlays.read(cx).is_open(MenuKind::ViewOptions)
                || self.overlays.read(cx).is_open(MenuKind::Account)
                || self.overlays.read(cx).is_open(MenuKind::SessionRow);
            if open {
                self.overlays.update(cx, |overlays, cx| overlays.close_menu(cx));
            }
        }
        match intent {
            SidebarIntent::Search => self.open_search(window, cx),
            SidebarIntent::NewSession => self.new_session(window, cx),
            SidebarIntent::Nav(id) => match id.as_ref() {
                NAV_PROJECTS => self.open_projects(false, window, cx),
                NAV_SKILLS => self.open_skills(cx),
                NAV_NEEDS_YOU => self.toggle_needs_you_filter(cx),
                _ => {}
            },
            SidebarIntent::Select(id) => {
                // A sidebar click never arms the reveal: the clicked row is
                // under the cursor (owner round 6). The pane already
                // dismissed the hover card: the click is the row's.
                self.resume_quiet(id.to_string(), window, cx);
            }
            SidebarIntent::Toggle(id) => self.toggle_group(id.to_string(), cx),
            // The pane flipped its own disclosure; the root only closes menus.
            SidebarIntent::TogglePinned => {}
            SidebarIntent::ViewOptions => {
                if !self.just_dismissed(MenuKind::ViewOptions, cx) {
                    self.open_menu(MenuKind::ViewOptions, cx);
                }
            }
            SidebarIntent::ViewOption(option) => self.apply_view_option(option, cx),
            SidebarIntent::MenuBackdrop => {}
            SidebarIntent::DismissMenus => {
                // Remember what the press closed: when the press was on the
                // menu's own trigger, the click that follows must not reopen it.
                let closed = self.overlays.read(cx).menu().map(|menu| menu.kind);
                self.overlays.update(cx, |overlays, cx| {
                    overlays.stack.note_pressed_shut(closed.map(crate::overlay_stack::Trigger::Menu));
                    overlays.close_menu(cx);
                });
            }
            SidebarIntent::AddProject => self.open_projects(false, window, cx),
            // A group row's actions: `+` starts a session in that project
            // (and makes it current), `…` opens its project menu. On "Other
            // workspaces" `+` has nowhere to start, so it offers adoption.
            SidebarIntent::GroupAction(id, GroupAction::New) => {
                if id.as_ref() == crate::sidebar::OTHER_GROUP {
                    self.open_projects(false, window, cx);
                } else {
                    self.new_session_in(Some(id.to_string()), window, cx);
                }
            }
            SidebarIntent::GroupAction(id, GroupAction::Menu) => {
                // The pane noted the pointer as the head's `…` seat
                // (`SidebarPane::group_menu_bounds`) before it asked.
                if id.as_ref() == crate::sidebar::OTHER_GROUP {
                    self.open_project_menu(None, false, cx);
                } else {
                    self.open_project_menu(Some(id.to_string()), false, cx);
                }
            }
            SidebarIntent::GroupAction(id, GroupAction::ToggleMore) | SidebarIntent::ToggleMore(id) => {
                self.toggle_expanded(id.to_string(), cx);
            }
            // The row's `⋯`: its menu opens under it, or closes when it is
            // that row's menu already (the press that closed it is not a
            // reopen).
            SidebarIntent::RowAction(id, RowAction::More) => {
                let open = self.open_row_menu(cx).as_deref() == Some(id.as_ref());
                if !open && !self.just_dismissed(MenuKind::SessionRow, cx) {
                    self.open_row_menu_for(id.to_string(), cx);
                } else {
                    self.overlays.update(cx, |overlays, cx| overlays.close_menu(cx));
                }
            }
            // The row's "N running" (docs/82 6.5): the session opens (as a
            // row click opens it) with the pane on its Tasks.
            SidebarIntent::RowAction(id, RowAction::Tasks) => {
                self.open_session_tasks(id.to_string(), window, cx);
            }
            // The row's "N pages" (docs/82 8.4): its Page, or Browse on its
            // task folder when it owns several.
            SidebarIntent::RowAction(id, RowAction::Pages) => {
                self.open_session_pages(id.to_string(), window, cx);
            }
            // A right-click on a row: the same menu at the pointer (docs/82
            // 8.5), replacing whatever menu stood open.
            SidebarIntent::RowMenuAt(id, at) => self.open_row_menu_at(id.to_string(), Some(at), cx),
            SidebarIntent::MoveToProject(id, project) => {
                self.move_session_to_project(&id, project.map(|p| p.to_string()), cx);
            }
            SidebarIntent::RowAction(id, action) => {
                match action {
                    RowAction::Rename => self.start_rename(id.to_string(), window, cx),
                    RowAction::Pin => self.toggle_pin(id.to_string(), cx),
                    RowAction::Fork => self.open_then(id.to_string(), super::row_menu::RowFollowUp::Fork, window, cx),
                    RowAction::ContinueIn => {
                        self.open_then(id.to_string(), super::row_menu::RowFollowUp::ContinueIn, window, cx)
                    }
                    RowAction::Reveal => {
                        self.reveal_session_folder(&id, cx);
                    }
                    RowAction::Delete => self.open_delete_dialog(id.to_string(), cx),
                    // One archive affordance: a listed session asks first,
                    // an archived one goes straight back.
                    RowAction::Archive => {
                        let archived =
                            self.session_list.read(cx).sessions.iter().find(|e| e.id == id.as_ref()).is_some_and(|e| e.archived);
                        if archived {
                            self.unarchive_session(id.to_string(), cx);
                        } else {
                            self.open_archive_dialog(id.to_string(), cx);
                        }
                    }
                    _ => {}
                }
            }
            SidebarIntent::Rename(id, text) => {
                self.rename_session(id.to_string(), Some(text.to_string()), cx);
                self.arm_composer_focus(cx);
            }
            SidebarIntent::RenameCancel => {
                self.overlays.update(cx, |o, cx| {
                    o.close(crate::overlay_stack::LayerKind::RenameSession, cx);
                });
                self.arm_composer_focus(cx);
            }
            SidebarIntent::Account => {
                if !self.just_dismissed(MenuKind::Account, cx) {
                    self.open_menu(MenuKind::Account, cx);
                }
            }
            SidebarIntent::AccountAction(action) => {
                self.overlays.update(cx, |overlays, cx| overlays.close_menu(cx));
                match action {
                    AccountAction::Settings => self.open_settings(0, cx),
                    AccountAction::Providers => self.open_providers(cx),
                    // The menu carries no sign-out row (each provider signs
                    // out on its card); the intent, if anything sends it,
                    // goes where those cards are.
                    AccountAction::SignOut => self.open_providers(cx),
                }
            }
            // The column's own hover state: an overlay-hosted menu's report
            // goes back to the pane, which settles it without the root.
            SidebarIntent::Hover(..) | SidebarIntent::HoverGroup(_) | SidebarIntent::HoverProjectsHeader(_) => {
                self.sidebar_pane.update(cx, |pane, cx| pane.intent(intent, window, cx));
                return;
            }
            SidebarIntent::Browse(intent) => self.browse_intent(intent, window, cx),
        }
        cx.notify();
    }

    #[cfg(feature = "probe")]
    /// Whether `kind` was dismissed by the press of the click now arriving:
    /// a trigger's own click after an outside-press close toggles it shut,
    /// never back open.
    /// `sidebar-hover:<id>`: stand the sidebar's pointer state on `id`'s
    /// row, as its own hover report would (the hover card is not armed).
    /// An id with no session fails the step.
    pub(crate) fn step_sidebar_hover(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let id = rest.trim();
        self.sidebar_pane.update(cx, |pane, cx| pane.hover_step(id, cx))
    }

    #[cfg(feature = "probe")]
    /// `row-menu:<id>`: open `id`'s row menu (never a toggle). An id with
    /// no session fails the step.
    pub(crate) fn step_row_menu(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let id = rest.trim();
        if !self.session_list.read(cx).sessions.iter().any(|entry| entry.id == id) {
            return Err(format!("no session row `{id}`"));
        }
        self.open_row_menu_for(id.to_owned(), cx);
        Ok(())
    }

    /// The session whose row menu (`⋯`) stands open, if one does.
    pub(crate) fn open_row_menu(&self, cx: &gpui::App) -> Option<String> {
        self.overlays.read(cx).menu().filter(|menu| menu.kind == MenuKind::SessionRow).and_then(|menu| menu.session.clone())
    }

    /// Open `session`'s row menu under its `⋯`, replacing whatever menu
    /// was open.
    pub(crate) fn open_row_menu_for(&mut self, session: String, cx: &mut Context<Self>) {
        self.open_row_menu_at(session, None, cx);
    }

    /// Open `session`'s row menu — at `at` for a right-click, else under its
    /// `⋯` — replacing whatever menu was open.
    pub(crate) fn open_row_menu_at(&mut self, session: String, at: Option<gpui::Point<gpui::Pixels>>, cx: &mut Context<Self>) {
        if let Some(view) = self.active.clone() {
            view.update(cx, |view, cx| view.close_plus_menu(cx));
        }
        let menu = match at {
            Some(at) => crate::overlays::Menu::session_row_at(session, at),
            None => crate::overlays::Menu::session_row(session),
        };
        self.overlays.update(cx, |overlays, cx| overlays.open(menu, cx));
        // Outside the draw: the column takes the open menu on this frame.
        self.push_sidebar_facts(cx);
    }

    /// Whether this click on `kind`'s trigger is the tail of the press that
    /// just closed `kind`'s menu ([`crate::overlay_stack::OverlayStack::note_pressed_shut`]).
    fn just_dismissed(&mut self, kind: MenuKind, cx: &mut Context<Self>) -> bool {
        self.overlays
            .update(cx, |overlays, _| overlays.stack.pressed_shut(crate::overlay_stack::Trigger::Menu(kind)))
            .is_some_and(|ago| ago < crate::overlay_stack::REPRESS)
    }

    /// One row of the view-options menu (or its Group by / Sort by
    /// submenus). Toggles keep the menu open, so the check is seen to
    /// change; a submenu choice and Clear empty close it.
    fn apply_view_option(&mut self, option: ViewOption, cx: &mut Context<Self>) {
        match option {
            ViewOption::GroupBy(key) => {
                self.set_group_by(
                    match key {
                        GroupKey::Project => crate::layout::GroupBy::Project,
                        GroupKey::Date => crate::layout::GroupBy::Date,
                    },
                    cx,
                );
                self.overlays.update(cx, |overlays, cx| overlays.close_menu(cx));
            }
            ViewOption::SortBy(key) => {
                self.set_sort_by(
                    match key {
                        SortKey::Recent => crate::layout::SortBy::Recent,
                        SortKey::Created => crate::layout::SortBy::Created,
                        SortKey::Name => crate::layout::SortBy::Name,
                    },
                    cx,
                );
                self.overlays.update(cx, |overlays, cx| overlays.close_menu(cx));
            }
            ViewOption::ToggleEmpty => {
                self.session_list.update(cx, |list, _| list.show_empty = !list.show_empty);
                self.invalidate_list(cx);
            }
            ViewOption::ToggleArchived => {
                self.session_list.update(cx, |list, _| list.show_archived = !list.show_archived);
                self.invalidate_list(cx);
            }
            ViewOption::ToggleSearchAll => self.toggle_search_scope(cx),
            ViewOption::ClearEmpty => {
                self.overlays.update(cx, |overlays, cx| overlays.close_menu(cx));
                self.clear_empty(cx);
            }
        }
    }

    /// Browse mode's intents, routed to the browser's own handlers.
    fn browse_intent(&mut self, intent: BrowseIntent, window: &mut Window, cx: &mut Context<Self>) {
        match intent {
            BrowseIntent::Address => {
                let url = self.browse_centre_state(cx).map(|state| state.read(cx).url().to_string()).unwrap_or_default();
                self.focus_browse_address_with(&url, window, cx);
            }
            BrowseIntent::SiteInfo => {
                let open = self.browse_centre_state(cx).is_some();
                self.overlays.update(cx, |o, cx| o.set(crate::overlay_stack::Layer::SiteInfo, open, cx));
            }
            BrowseIntent::Bookmark => self.edit_shown_bookmark(window, cx),
            BrowseIntent::Back => self.browse_go_back(cx),
            BrowseIntent::Forward => self.browse_go_forward(cx),
            BrowseIntent::Reload => {
                if let Some(id) = self.browser.read(cx).browse_centre_shown(cx) {
                    self.reload_browse_tab(id, window, cx);
                }
            }
            BrowseIntent::NewTab => self.new_browse_sidebar_tab(window, cx),
            BrowseIntent::PageMenu(anchor) => self.toggle_browse_page_menu(anchor, window, cx),
            BrowseIntent::Downloads(anchor) => self.toggle_browse_downloads(anchor, cx),
            BrowseIntent::Page(action) => self.browse_page_action(action, window, cx),
            BrowseIntent::OpenPin(id) | BrowseIntent::SelectTab(id) => {
                if let Some(id) = browse_tab_id(&id) {
                    self.browse_sidebar_tab(id, false, window, cx);
                }
            }
            BrowseIntent::CloseTab(id) => {
                if let Some(id) = browse_tab_id(&id) {
                    self.browse_sidebar_tab(id, true, window, cx);
                }
            }
            BrowseIntent::ToggleFolder(id) => {
                if self.browser.update(cx, |browser, _| browser.tabs.toggle_folder(&id)) {
                    self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
                }
            }
            BrowseIntent::OpenTaskChat(session) => self.browse_task_folder_action(&session, TaskFolderAction::OpenChat, window, cx),
            // The footer's context (docs/82 8.4): back to Sessions.
            BrowseIntent::NeedsYou => self.back_to_needs_you(window, cx),
            BrowseIntent::LastSession(session) => self.back_to_session(session.to_string(), window, cx),
            // The column's own hover state (see `sidebar_intent`).
            BrowseIntent::HoverTodayHeader(on) => {
                let intent = SidebarIntent::Browse(BrowseIntent::HoverTodayHeader(on));
                self.sidebar_pane.update(cx, |pane, cx| pane.intent(intent, window, cx));
            }
            BrowseIntent::Ask => self.browse_ask_page(window, cx),
            BrowseIntent::SwitchSpace(index) => self.browse_switch_space(index, window, cx),
            BrowseIntent::NewSpace => self.start_browse_naming(BrowseNaming::Space(None), "", window, cx),
            BrowseIntent::NewFolder => self.browse_new_folder(None, window, cx),
            BrowseIntent::CloseAll => self.browse_close_all(window, cx),
            BrowseIntent::OpenMenu(menu) => self.open_browse_menu(menu, cx),
            BrowseIntent::CloseMenu => self.close_browse_menu(cx),
            BrowseIntent::Tab(id, action) => {
                if let Some(id) = browse_tab_id(&id) {
                    self.browse_tab_action(id, action, window, cx);
                }
            }
            BrowseIntent::Folder(id, action) => self.browse_folder_action(&id, action, window, cx),
            BrowseIntent::TaskFolder(session, action) => self.browse_task_folder_action(&session, action, window, cx),
            BrowseIntent::Space(id, action) => self.browse_space_action(&id, action, window, cx),
            BrowseIntent::Name(target, text) => self.commit_browse_name(target, &text, cx),
            BrowseIntent::NameCancel => self.cancel_browse_name(cx),
        }
    }
    pub(crate) fn render_row_detail(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (element, reason) = self.build_row_detail(cx);
        let shown = element.is_some();
        // Diagnosis only (`BAAZ_HOVER_TRACE=1`): one line per result
        // change, never per frame — the root calls this every frame.
        // Written without a notify: the trace's own memory, not state the
        // column draws.
        let changed = self.sidebar_pane.update(cx, |pane, _| {
            let changed = pane.detail.trace_last_shown != Some(shown);
            pane.detail.trace_last_shown = Some(shown);
            changed
        });
        if changed {
            let detail = &self.sidebar_pane.read(cx).detail;
            crate::hover_trace!(
                "render result={} reason={reason} id={:?} hovered={:?} shown={} trigger={}",
                if shown { "Some" } else { "None" },
                detail.forced.clone().or_else(|| detail.hovered_row.clone()),
                detail.hovered_row,
                detail.shown,
                detail.trigger.is_some()
            );
        }
        element
    }

    /// The card builder behind [`Self::render_row_detail`], with the reason
    /// a missing card is missing: `not-shown` (still inside the delay),
    /// `no-hovered-row`, `no-trigger`, `no-sidebar-edge`, or the forced
    /// pin's own misses and unknown rows under `unknown-row` / `forced-*`.
    fn build_row_detail(&mut self, cx: &mut Context<Self>) -> (Option<AnyElement>, &'static str) {
        let detail = &self.sidebar_pane.read(cx).detail;
        let (forced, selected_row_bounds, shown, hovered_row, trigger, pane_bounds) = (
            detail.forced.clone(),
            detail.selected_row_bounds.clone(),
            detail.shown,
            detail.hovered_row.clone(),
            detail.trigger,
            detail.pane_bounds,
        );
        let (id, row) = match forced {
            Some(id) => {
                let Some((known, bounds)) = selected_row_bounds else {
                    return (None, "forced-no-bounds");
                };
                if known != id {
                    return (None, "forced-id-mismatch");
                }
                (id, bounds)
            }
            None => {
                if !shown {
                    return (None, "not-shown");
                }
                let Some(hovered) = hovered_row else {
                    return (None, "no-hovered-row");
                };
                let Some(row) = trigger else {
                    return (None, "no-trigger");
                };
                (hovered, row)
            }
        };
        // The seat's other half: the laid-out pane's right edge. Before the
        // first prepaint there is no edge, so there is no card this frame.
        let Some(edge) = pane_bounds else {
            return (None, "no-sidebar-edge");
        };
        let sidebar_right = edge.origin.x + edge.size.width;
        let visible = self.visible_sessions(cx);
        // The visible rows first, then the whole list: a card pinned to a
        // filtered-out row still names it honestly.
        let Some(entry) = visible
            .iter()
            .find(|entry| entry.id == id)
            .or_else(|| self.session_list.read(cx).sessions.iter().find(|entry| entry.id == id))
        else {
            return (None, "unknown-row");
        };
        let now = crate::sidebar::grouping_now(&visible);
        // The open session's own fold is fresher than the joined row: no
        // wire event will ever sync it on a `--replay` window, and the
        // pending words live only there. A presentation-local overlay, never
        // a write to the list — the wire sync owns the entries.
        let mut entry = entry.clone();
        if let Some(view) = self.active.clone() {
            let view = view.read(cx);
            if view.rt(cx).session_id == id {
                let (approval, question) = view.rt(cx).row_pending();
                if approval.is_some() {
                    entry.approval_command = approval;
                }
                if question.is_some() {
                    entry.pending_question = question;
                }
                if entry.last_ask.as_deref().map(str::trim).is_none_or(|s| s.is_empty()) {
                    entry.last_ask = view.rt(cx).last_user_text();
                }
                if entry.description.trim().is_empty() {
                    if let Some(summary) = view.rt(cx).last_summary_text() {
                        entry.description = summary;
                    }
                }
            }
        }
        let data = entry.detail_data(now);
        let card_id: ElementId = (ElementId::from("row-detail"), SharedString::from(id.clone())).into();
        let mut card = aui::nav::session_detail(card_id);
        if let Some(title) = data.title {
            card = card.title(title);
        }
        if let Some(ask) = data.ask {
            card = card.ask(ask);
        }
        if let Some(reply) = data.reply {
            card = card.reply(reply);
        }
        if let Some(status) = data.status {
            card = card.status(status.kind, status.detail);
        }
        if let Some(project) = data.project {
            card = card.project(project);
        }
        if let Some(branch) = data.branch {
            card = card.branch(branch);
        }
        if let Some(turns) = data.turns {
            card = card.turns(turns);
        }
        if let Some(updated) = data.updated {
            card = card.updated(updated);
        }
        if let Some(workspace) = data.workspace {
            card = card.workspace(workspace);
        }
        if let Some(question) = data.pending_question {
            card = card.pending_question(question);
        }
        if let Some(approval) = data.pending_approval {
            card = card.pending_approval(approval);
        }
        // `anchored_session_detail_at_sidebar` takes the card itself, so no
        // wrapper rides along: the card carries no click handler of its own,
        // and a click that lands on it is a no-op rather than a row action.
        // That corner is nearly unreachable — reaching the card means leaving
        // the row, and leaving closes it — while every row click still
        // lands on its row (the card hangs beside the sidebar, never over
        // it) and dismisses through the select/action listeners; the row's
        // own leave report and scroll close it.
        (
            Some(anchored_session_detail_at_sidebar(row, sidebar_right, card).into_any_element()),
            "shown",
        )
    }

    /// Dismiss an open group-row project menu, if any. The header menu
    /// seats from the fixed crumb and is never dismissed by list movement.
    fn dismiss_group_menu(&mut self, cx: &mut Context<Self>) {
        if self
            .overlays
            .read(cx)
            .menu()
            .is_some_and(|menu| menu.kind == MenuKind::Project && !menu.project_header)
        {
            self.overlays.update(cx, |overlays, cx| overlays.close_menu(cx));
        }
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
    /// commit path is unchanged. The header title takes one element built
    /// here; the sidebar row's field is the screen's own, handed the same
    /// `rename` state (`rename_textarea` in the sidebar pane's render),
    /// because the library builds that row more than once per frame.
    pub(crate) fn rename_field(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        Self::rename_editor(
            &self.rename,
            cx.aui().colors,
            self.rename.focus_handle(cx).is_focused(window),
            cx.entity().downgrade(),
        )
    }

    /// One rename editor, built from its pieces with no view context: the
    /// per-build half of [`Self::rename_field`]. Entity-agnostic on purpose —
    /// the virtual list calls the row's builder with `(&mut Window, &mut
    /// App)` inside layout, where there is no `Context` to bind a listener
    /// with, so the confirm action upgrades the weak handle instead. Builds
    /// only, never notifies: a builder runs inside layout.
    fn rename_editor(
        rename: &Entity<TextareaState>,
        p: Palette,
        focused: bool,
        baaz: WeakEntity<Self>,
    ) -> AnyElement {
        div()
            .w_full()
            .key_context(RENAME_CONTEXT)
            .on_action(move |_: &ConfirmRename, window, cx| {
                let _ = baaz.update(cx, |this, cx| this.commit_rename(window, cx));
            })
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
                    .border_color(if focused { p.accent_ring } else { p.line })
                    .bg(p.surface_1)
                    // The editor keeps its natural line box (`h_auto`): the
                    // library's 20 px recipe stands its glyphs on the bottom
                    // edge of a 22 px wrapper and clips their descenders. The
                    // wrapper is the fixed box that keeps the row's height;
                    // the editor centres in it and may overhang the border by
                    // a pixel or two of line box, which draws nothing.
                    .child(dense_field(rename).h_auto().whitespace_nowrap().overflow_x_hidden()),
            )
            .into_any_element()
    }
}
