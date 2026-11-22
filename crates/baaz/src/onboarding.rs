//! The first run after Connect (docs/82 8.7; docs/81 F A4, A6): one calm
//! step asks where the person will work, and the composer opens there with
//! the window's four regions named once; the first page the person opens
//! in Browse is followed by a one-time offer to make Baaz the default
//! browser.
//!
//! - **The folder step** is the connect screen's second step
//!   ([`crate::connect::ConnectStep::Folder`]), shown only after the first
//!   run's Continue (or Skip): "Where will you work?" — choose a folder
//!   (the folder panel, as Add project), drop one on the step, or "Not
//!   now". A folder becomes a project and a new session opens in it on the
//!   default provider; "Not now" opens the new session where any new
//!   session with no folder chosen starts — the current project when the
//!   launch adopted one, else `~/baaz-sessions` on **every** provider
//!   (docs/82 0.9; `Harness::unfiled_workspace`), never `/`.
//! - **The regions strip** ([`regions_strip`]) stands at the top of the
//!   session column while [`crate::layout::Onboarding::regions`] is `Due` —
//!   set by the folder step, so only a first run ever sees it — and names
//!   the sidebar, the conversation and its composer, the workspace pane and
//!   Browse. "Got it" or Escape ends it for good.
//! - **The default-browser offer** ([`default_browser_strip`]) is a row in
//!   the chrome above the Browse page, like the permission strip, while
//!   [`crate::layout::Onboarding::default_browser`] is `Due`: set after the
//!   first page a person tab loads in front in Browse
//!   ([`Harness::note_browse_used`]), never in a capture (a verb asks for it
//!   there), and never when Baaz already is the default. "Make default"
//!   runs the Settings row's own [`crate::default_browser::make_baaz_default`];
//!   either answer ends it for good. Settings › Browser keeps its row.
//!
//! The state is the layout store's ([`crate::layout::Onboarding`]): a
//! relaunch keeps what was shown, and nothing shows twice.

use std::path::PathBuf;

use aui::data::button;
use aui::util::{h_flex, v_flex};
use aui_icons::{icon, IconName};
use aui_tokens::{scale, ActiveAui, AuiStyled, TextRole};
use gpui::{div, prelude::*, px, AnyElement, App, Context, ExternalPaths, SharedString, Window};

use crate::app::Harness;
use crate::layout::{LayoutGlobal as _, Once};

/// The folder step's column width: the connect screen's card width, so
/// the two steps read as one screen.
const STEP_W: f32 = 480.0;

/// The drop target's height on the folder step.
const DROP_H: f32 = 120.0;

/// What the folder step asks of the root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FolderIntent {
    /// "Choose a folder…": the folder panel.
    Choose,
    /// A folder dropped on the step.
    Drop(PathBuf),
    /// "Not now": the composer, where a session with no folder chosen
    /// starts.
    Skip,
}

/// The first folder among dropped `paths`: a dropped file is not a place
/// to work, and the step takes one folder.
pub(crate) fn dropped_folder(paths: &[PathBuf]) -> Option<PathBuf> {
    paths.iter().find(|path| path.is_dir()).cloned()
}

/// The line under the folder step's buttons: where "Not now" starts the
/// session — the project the launch adopted (`current`), else the default
/// workspace (`default_workspace`, `~`-abbreviated).
pub(crate) fn skip_line(current: Option<&str>, default_workspace: &str) -> String {
    match current {
        Some(name) => format!("Not now keeps you in {name}. Add a folder any time from the sidebar."),
        None => format!("Not now starts in {default_workspace}. Add a folder any time from the sidebar."),
    }
}

/// The folder step (docs/82 8.7): "Where will you work?", a drop target,
/// "Not now" and "Choose a folder…", and the line saying where "Not now"
/// starts. `on` hears each intent.
pub(crate) fn folder_step(
    skip_line: String,
    on: impl Fn(FolderIntent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let p = cx.aui().colors;
    let on = std::rc::Rc::new(on);
    let choose = {
        let on = on.clone();
        move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| on(FolderIntent::Choose, window, cx)
    };
    let skip = {
        let on = on.clone();
        move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| on(FolderIntent::Skip, window, cx)
    };
    let target = v_flex()
        .id("first-run-drop")
        .w_full()
        .h(px(DROP_H))
        .items_center()
        .justify_center()
        .gap(px(scale::SP_3))
        .rounded(px(scale::R_LG))
        .border_1()
        .border_color(p.line_strong)
        .bg(p.surface_2)
        .role(gpui::Role::Group)
        .aria_label("Drop a folder here to work in it")
        .drag_over::<ExternalPaths>(move |style, _, _, _| style.border_color(p.accent_ring))
        .child(icon(IconName::Folder).size(px(scale::ICON_DEFAULT)).color(p.fg_muted))
        .child(div().text_role(TextRole::Body).text_color(p.fg_muted).child("Drop a folder here"));
    v_flex()
        .id("first-run-folder")
        .size_full()
        .items_center()
        .justify_center()
        .bg(p.surface_1)
        .role(gpui::Role::Group)
        .aria_label("Where will you work?")
        // The whole step takes a drop, not just its target.
        .on_drop({
            move |paths: &ExternalPaths, window, cx| {
                if let Some(folder) = dropped_folder(paths.paths()) {
                    on(FolderIntent::Drop(folder), window, cx);
                }
            }
        })
        .child(
            v_flex()
                .w(px(STEP_W))
                .gap(px(scale::SP_5))
                .child(
                    v_flex()
                        .gap(px(scale::SP_2))
                        .child(div().text_role(TextRole::Title).text_color(p.ink).child("Where will you work?"))
                        .child(
                            div()
                                .text_role(TextRole::Body)
                                .text_color(p.fg_muted)
                                .child("Pick the folder of a project. Its sessions, changes and files stay together, and the agent works in it."),
                        ),
                )
                .child(target)
                .child(
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap(px(scale::SP_3))
                        .child(
                            button("first-run-skip", "Not now")
                                .ghost()
                                .accessibility_label("Not now: open a new session without choosing a folder")
                                .on_click(skip),
                        )
                        .child(
                            button("first-run-choose", "Choose a folder…")
                                .primary()
                                .icon(IconName::Folder)
                                .accessibility_label("Choose a folder to work in")
                                .on_click(choose),
                        ),
                )
                .child(div().text_role(TextRole::Caption).text_color(p.fg_muted).child(skip_line)),
        )
        .into_any_element()
}

/// One region the strip names: its glyph, its name, and what it holds.
struct Region {
    glyph: IconName,
    name: &'static str,
    holds: &'static str,
}

/// The four regions, in reading order across the window: the sidebar on
/// the left, the conversation in the middle, the workspace pane on the
/// right, and Browse (the sidebar's mode switch, ⌥⌘B).
const REGIONS: [Region; 4] = [
    Region { glyph: IconName::Sidebar, name: "Sidebar", holds: "Your projects and their sessions, on the left." },
    Region { glyph: IconName::Edit, name: "Conversation", holds: "Ask here: the composer is at the bottom." },
    Region {
        glyph: IconName::PanelRight,
        name: "Workspace pane",
        holds: "Changes · Files · Page · Terminal · Tasks, from the header's buttons.",
    },
    Region { glyph: IconName::Globe, name: "Browse", holds: "Your own tabs: the globe at the top of the sidebar, ⌥⌘B." },
];

/// The strip's accessible name, also what a screen reader hears for the
/// whole of it.
pub(crate) const REGIONS_LABEL: &str = "Where things are in the window";

/// The four regions named once (docs/82 8.7): one strip at the top of the
/// session column on the first composer, the four side by side, and "Got
/// it". `dismiss` ends it for good (Escape does too, through the root's
/// cancel).
pub(crate) fn regions_strip(dismiss: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static, cx: &App) -> AnyElement {
    let p = cx.aui().colors;
    let mut row = h_flex().flex_1().min_w_0().items_start().gap(px(scale::SP_6));
    for (ix, region) in REGIONS.iter().enumerate() {
        row = row.child(
            v_flex()
                .id(("regions-strip-region", ix))
                .flex_1()
                .min_w_0()
                .gap(px(scale::SP_1))
                .role(gpui::Role::Label)
                .aria_label(SharedString::from(format!("{}: {}", region.name, region.holds)))
                .child(
                    h_flex()
                        .gap(px(scale::SP_3))
                        .items_center()
                        .child(icon(region.glyph).size(px(scale::ICON_SM)).color(p.fg_muted))
                        .child(div().text_role(TextRole::Body).text_color(p.ink).child(region.name)),
                )
                .child(div().text_role(TextRole::Caption).text_color(p.fg_muted).child(region.holds)),
        );
    }
    h_flex()
        .id("regions-strip")
        .w_full()
        .flex_none()
        .items_center()
        .gap(px(scale::SP_6))
        .px(px(scale::SP_6))
        .py(px(scale::SP_4))
        .bg(p.surface_1)
        .border_b_1()
        .border_color(p.line)
        .role(gpui::Role::Group)
        .aria_label(REGIONS_LABEL)
        .child(row)
        .child(
            button("regions-strip-done", "Got it")
                .sm()
                .accessibility_label("Got it: hide where things are")
                .on_click(dismiss),
        )
        .into_any_element()
}

/// The default-browser offer's words.
pub(crate) const OFFER_TEXT: &str = "Make Baaz your default browser? Links you open in other apps would open here, in Browse.";

/// The default-browser offer (docs/82 8.7): one row in the chrome above
/// the Browse page, like the permission strip, with "Not now" and "Make
/// default". `answer(true)` makes Baaz the default; either answer ends the
/// offer for good.
pub(crate) fn default_browser_strip(answer: impl Fn(bool, &mut Window, &mut App) + 'static, cx: &App) -> AnyElement {
    let p = cx.aui().colors;
    let answer = std::rc::Rc::new(answer);
    let later = {
        let answer = answer.clone();
        move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| answer(false, window, cx)
    };
    let make = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| answer(true, window, cx);
    h_flex()
        .id("browse-default-offer")
        .w_full()
        .flex_none()
        .items_center()
        .h(px(scale::H_XL))
        .px(px(scale::SP_3))
        .gap(px(scale::SP_2))
        .bg(p.surface_1)
        .border_b_1()
        .border_color(p.line)
        .role(gpui::Role::Group)
        .aria_label("Default browser")
        .child(icon(IconName::Globe).size(px(scale::ICON_SM)).color(p.fg_muted))
        .child(
            div()
                .id("browse-default-offer-text")
                .flex_1()
                .min_w_0()
                .truncate()
                .ui(TextRole::Body)
                .text_color(p.ink)
                .role(gpui::Role::Label)
                .aria_label(OFFER_TEXT)
                .child(OFFER_TEXT),
        )
        .child(
            button("browse-default-later", "Not now")
                .sm()
                .ghost()
                .accessibility_label("Not now: keep your default browser")
                .on_click(later),
        )
        .child(
            button("browse-default-make", "Make default")
                .sm()
                .primary()
                .accessibility_label("Make Baaz your default browser")
                .on_click(make),
        )
        .into_any_element()
}

/// Whether the person's first use of Browse is the moment for the offer:
/// it has not been asked for yet, this is no capture, and the handler is
/// known (`handler`: the cached `https` handler; `None` while unread) and
/// is not Baaz. `Some(false)` when the handler says the offer is moot
/// (Baaz already is the default): the offer is done without showing.
pub(crate) fn offer_due(state: Once, capture: bool, handler: Option<Option<&str>>) -> Option<bool> {
    if state != Once::NotYet || capture {
        return None;
    }
    let handler = handler?;
    Some(crate::default_browser::offers_make_default(handler))
}

impl Harness {
    /// The regions strip for this frame: on the session column while it is
    /// due (docs/82 8.7).
    pub(crate) fn render_regions_strip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if crate::layout::Layout::get(cx).onboarding.regions != Once::Due {
            return None;
        }
        let dismiss = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
            this.dismiss_regions_strip(cx);
        });
        Some(regions_strip(dismiss, cx))
    }

    /// End the regions strip for good ("Got it", Escape). `false` when it
    /// was not showing.
    pub(crate) fn dismiss_regions_strip(&mut self, cx: &mut Context<Self>) -> bool {
        if crate::layout::Layout::get(cx).onboarding.regions != Once::Due {
            return false;
        }
        // The root observes the layout store: the update redraws it.
        crate::layout::Layout::update(cx, |layout| layout.onboarding.regions = Once::Done);
        true
    }

    /// The first run's folder step answered (docs/82 8.7): the screen goes,
    /// a chosen or dropped `folder` becomes the current project, the
    /// regions strip is due, and a new session opens — in the folder, or
    /// where a session with no folder chosen starts (the current project,
    /// else `~/baaz-sessions` on every provider; never `/`).
    pub(crate) fn finish_first_run(&mut self, folder: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        self.connect_screen.update(cx, |screen, cx| screen.dismiss(cx));
        if let Some(folder) = folder {
            self.adopt_root(&folder, cx);
        }
        crate::layout::Layout::update(cx, |layout| {
            if layout.onboarding.regions == Once::NotYet {
                layout.onboarding.regions = Once::Due;
            }
        });
        self.new_session(window, cx);
        // Links from other apps that waited behind the screen open now.
        self.drain_external_soon(cx);
        // The shell is up: a session it restored warms its Browser pane.
        self.maybe_idle_prewarm(cx);
    }

    /// One intent from the folder step.
    pub(crate) fn on_folder_intent(&mut self, intent: FolderIntent, window: &mut Window, cx: &mut Context<Self>) {
        match intent {
            FolderIntent::Drop(folder) => self.finish_first_run(Some(folder), window, cx),
            FolderIntent::Skip => self.finish_first_run(None, window, cx),
            FolderIntent::Choose => self.choose_first_folder(cx),
        }
    }

    /// The folder panel for the first run's step: a chosen folder finishes
    /// the step; a cancelled panel leaves the step standing.
    fn choose_first_folder(&mut self, cx: &mut Context<Self>) {
        // Brought forward first so the app-modal panel never opens behind
        // another app (as Add project does).
        cx.activate(true);
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(folder) = paths.into_iter().next() else { return };
            let _ = this.update(cx, |this, cx| {
                this.run_on_main(cx, move |this, window, cx| this.finish_first_run(Some(folder), window, cx));
            });
        }));
    }

    /// The default-browser offer for this frame: above the Browse page
    /// while it is due (docs/82 8.7).
    pub(crate) fn render_default_browser_offer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if crate::layout::Layout::get(cx).onboarding.default_browser != Once::Due {
            return None;
        }
        let answer = cx.listener(|this: &mut Self, make: &bool, _, cx| this.answer_default_browser_offer(*make, cx));
        Some(default_browser_strip(move |make, window, cx| answer(&make, window, cx), cx))
    }

    /// The offer answered: "Make default" runs the Settings row's own call
    /// (macOS confirms it); either answer ends the offer for good.
    pub(crate) fn answer_default_browser_offer(&mut self, make: bool, cx: &mut Context<Self>) {
        crate::layout::Layout::update(cx, |layout| layout.onboarding.default_browser = Once::Done);
        // Only a person's click changes the system default: never a test,
        // never a capture.
        if make && !cfg!(all(test, feature = "probe")) && !crate::runtime::captures() {
            if !crate::default_browser::make_baaz_default() {
                crate::baaz_log!("Couldn't change the default browser (macOS refused the request)");
            }
            self.browser.update(cx, |browser, cx| browser.await_default_browser(cx));
        }
    }

    /// A person tab's page has loaded in front in Browse: the first time,
    /// the default-browser offer becomes due — unless this is a capture,
    /// the handler is not read yet, or Baaz already is the default (then
    /// the offer is done without showing).
    pub(crate) fn note_browse_used(&mut self, cx: &mut Context<Self>) {
        let state = crate::layout::Layout::get(cx).onboarding.default_browser;
        let handler = self.browser.read(cx).default_browser_cache.as_ref().map(|inner| inner.as_deref());
        let Some(offer) = offer_due(state, crate::runtime::captures(), handler) else { return };
        let next = if offer { Once::Due } else { Once::Done };
        crate::layout::Layout::update(cx, |layout| layout.onboarding.default_browser = next);
    }

    #[cfg(feature = "probe")]
    /// `default-browser-offer`: the offer as the first use of Browse raises
    /// it (a capture never raises it by itself). An `Err` when Browse shows
    /// no page, or the offer was already raised or answered.
    pub(crate) fn step_default_browser_offer(&mut self, cx: &mut Context<Self>) -> crate::steps::StepResult {
        if self.browse_centre_state(cx).is_none() {
            return Err("Browse shows no page".to_owned());
        }
        if crate::layout::Layout::get(cx).onboarding.default_browser != Once::NotYet {
            return Err("the default-browser offer was already raised".to_owned());
        }
        crate::layout::Layout::update(cx, |layout| layout.onboarding.default_browser = Once::Due);
        Ok(())
    }
}

#[cfg(feature = "probe")]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drop_takes_the_first_folder_and_never_a_file() {
        let dir = std::env::temp_dir();
        let file = std::env::current_exe().expect("the test binary");
        assert_eq!(dropped_folder(&[file.clone(), dir.clone()]), Some(dir));
        assert_eq!(dropped_folder(&[file]), None, "a file is no place to work");
        assert_eq!(dropped_folder(&[]), None);
    }

    /// "Not now" says where the session starts: the adopted project, else
    /// the default workspace — never the launch folder.
    #[test]
    fn not_now_says_where_the_session_starts() {
        assert!(skip_line(Some("acme-web"), "~/baaz-sessions").contains("keeps you in acme-web"));
        assert!(skip_line(None, "~/baaz-sessions").contains("starts in ~/baaz-sessions"));
    }

    /// The offer waits for the handler, never shows in a capture, never
    /// twice, and is moot when Baaz already is the default.
    #[test]
    fn the_default_browser_offer_is_due_once_and_only_when_it_means_something() {
        assert_eq!(offer_due(Once::NotYet, false, Some(Some("com.apple.safari"))), Some(true));
        assert_eq!(offer_due(Once::NotYet, false, Some(Some("sh.baaz.app"))), Some(false), "already the default");
        assert_eq!(offer_due(Once::NotYet, false, None), None, "handler not read yet: wait");
        assert_eq!(offer_due(Once::NotYet, true, Some(Some("com.apple.safari"))), None, "never in a capture");
        assert_eq!(offer_due(Once::Due, false, Some(Some("com.apple.safari"))), None);
        assert_eq!(offer_due(Once::Done, false, Some(Some("com.apple.safari"))), None, "answered: never again");
    }

    /// The strip names four regions, the workspace pane with all five of
    /// its views (8.1 made the terminal one of them).
    #[test]
    fn the_strip_names_the_four_regions_once() {
        let names: Vec<&str> = REGIONS.iter().map(|region| region.name).collect();
        assert_eq!(names, ["Sidebar", "Conversation", "Workspace pane", "Browse"]);
        let pane = REGIONS[2].holds;
        for view in ["Changes", "Files", "Page", "Terminal", "Tasks"] {
            assert!(pane.contains(view), "the pane names {view}");
        }
    }
}
