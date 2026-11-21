//! The first run after Connect (docs/82 8.7; docs/81 F A4, A6): one calm
//! step asks where the person will work, and the composer opens there.
//!
//! The folder step is the connect screen's second step
//! ([`crate::connect::ConnectStep::Folder`]), shown only after the first
//! run's Continue (or Skip): "Where will you work?" — choose a folder (the
//! folder panel, as Add project), drop one on the step, or "Not now". A
//! folder becomes a project and a new session opens in it on the default
//! provider; "Not now" opens the new session where any new session with no
//! folder chosen starts — the current project when the launch adopted one,
//! else `~/baaz-sessions` on **every** provider (docs/82 0.9;
//! `Harness::unfiled_workspace`), never `/`.

use std::path::PathBuf;

use aui::data::button;
use aui::util::{h_flex, v_flex};
use aui_icons::{icon, IconName};
use aui_tokens::{scale, ActiveAui, AuiStyled, TextRole};
use gpui::{div, prelude::*, px, AnyElement, App, Context, ExternalPaths, Window};

use crate::app::Harness;

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

impl Harness {
    /// The first run's folder step answered (docs/82 8.7): the screen goes,
    /// a chosen or dropped `folder` becomes the current project, and a new
    /// session opens — in the folder, or
    /// where a session with no folder chosen starts (the current project,
    /// else `~/baaz-sessions` on every provider; never `/`).
    pub(crate) fn finish_first_run(&mut self, folder: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        self.connect_screen.update(cx, |screen, cx| screen.dismiss(cx));
        if let Some(folder) = folder {
            self.adopt_root(&folder, cx);
        }
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
}

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
}
