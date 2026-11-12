//! R4 "Import from Chrome" (board R4Daily 2a–2d): the sheet over Settings ›
//! Browser. The counts are read off the main thread (Chrome's History is
//! copied first, and can be large); the import itself writes `browser.db`
//! on the main thread in 500-row transactions. Chrome is only read.
//! Passwords are not offered (see `browser::chrome_import`).

use std::path::PathBuf;

use aui::data::{button, icon_button};
use aui::skills::{checkbox, CheckboxIntent, CheckboxState};
use aui_icons::IconName;
use aui_tokens::{scale, ActiveAui, AuiStyled};
use gpui::{div, prelude::*, px, AnyElement, Context, SharedString};
use gpui_kit::base::{h_flex, v_flex};

use crate::app::Harness;
use crate::browser::chrome_import::{self, ChromeProfile, Counts, Imported};

const SHEET_W: f32 = 500.0;
const PAD_X: f32 = 16.0;

/// Where the sheet stands.
pub(crate) enum Stage {
    Choose,
    Done(Imported),
}

/// The open sheet.
pub(crate) struct ChromeImportSheet {
    pub profiles: Vec<ChromeProfile>,
    pub selected: usize,
    /// The selected profile's counts, once read (`None` while reading).
    pub counts: Option<Counts>,
    pub bookmarks: bool,
    pub history: bool,
    pub stage: Stage,
    /// Bumped per count request, so a late answer for another profile drops.
    pub epoch: u64,
}

impl Harness {
    /// Settings › Browser › Your data › Import…: open the sheet on Chrome's
    /// first profile and count it in the background.
    pub(crate) fn open_chrome_import(&mut self, cx: &mut Context<Self>) {
        let profiles = chrome_import::chrome_root().map(|root| chrome_import::profiles(&root)).unwrap_or_default();
        self.chrome_import = Some(ChromeImportSheet { profiles, selected: 0, counts: None, bookmarks: true, history: true, stage: Stage::Choose, epoch: 0 });
        self.count_chrome_profile(cx);
        cx.notify();
    }

    pub(crate) fn close_chrome_import(&mut self, cx: &mut Context<Self>) {
        self.chrome_import = None;
        cx.notify();
    }

    fn count_chrome_profile(&mut self, cx: &mut Context<Self>) {
        let Some(sheet) = self.chrome_import.as_mut() else { return };
        sheet.counts = None;
        sheet.epoch += 1;
        let epoch = sheet.epoch;
        let Some(dir) = sheet.profiles.get(sheet.selected).map(|p| p.dir.clone()) else { return };
        let now = crate::browser::tabs::now_ms();
        cx.spawn(async move |this, cx| {
            let counts = cx.background_executor().spawn(async move { chrome_import::count(&dir, now) }).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(sheet) = this.chrome_import.as_mut().filter(|s| s.epoch == epoch) {
                    sheet.counts = Some(counts);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn run_chrome_import(&mut self, cx: &mut Context<Self>) {
        let Some(sheet) = self.chrome_import.as_ref() else { return };
        let Some(dir): Option<PathBuf> = sheet.profiles.get(sheet.selected).map(|p| p.dir.clone()) else { return };
        let (b, h) = (sheet.bookmarks, sheet.history);
        let started = std::time::Instant::now();
        let done = chrome_import::import(self.browser.db(), &dir, b, h, crate::browser::tabs::now_ms(), |_, _| {});
        crate::baaz_log!(
            "chrome import: {} bookmarks, {} history pages in {} ms ({})",
            done.bookmarks,
            done.history,
            started.elapsed().as_millis(),
            done.source
        );
        self.browser.history_changed();
        self.browser.bookmarks_changed();
        if let Some(sheet) = self.chrome_import.as_mut() {
            sheet.stage = Stage::Done(done);
        }
        cx.notify();
    }

    fn undo_chrome_import(&mut self, cx: &mut Context<Self>) {
        if let Some(ChromeImportSheet { stage: Stage::Done(done), .. }) = self.chrome_import.as_ref() {
            chrome_import::undo(self.browser.db(), &done.source);
            self.browser.history_changed();
            self.browser.bookmarks_changed();
            self.overlays.update(cx, |o, _| o.toast("Import undone", "What it added is gone; your own bookmarks and history stay."));
        }
        self.close_chrome_import(cx);
    }

    /// The sheet, over everything (like the sessions import sheet).
    pub(crate) fn render_chrome_import(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sheet = self.chrome_import.as_ref()?;
        let p = cx.aui().colors;
        let escape = cx.listener(|this: &mut Self, event: &gpui::KeyDownEvent, _, cx| {
            if event.keystroke.key == "escape" {
                this.close_chrome_import(cx);
                cx.stop_propagation();
            }
        });
        let title = "Import from Chrome";
        let close = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.close_chrome_import(cx));
        let head = h_flex()
            .w_full()
            .h(px(44.0))
            .flex_none()
            .items_center()
            .gap(px(10.0))
            .pl(px(PAD_X))
            .pr(px(8.0))
            .border_b_1()
            .border_color(p.line)
            .child(div().flex_1().text_px(scale::FS_13).semibold().text_color(p.ink).child(title))
            .child(icon_button("chrome-import-close", IconName::X).ghost().accessibility_label("Close Import from Chrome").on_click(move |e, w, cx| close(e, w, cx)));
        let mut body: Vec<AnyElement> = Vec::new();
        let mut actions: Vec<AnyElement> = Vec::new();
        let line = |text: String, faint: bool| {
            div().px(px(PAD_X)).pt(px(12.0)).text_px(scale::FS_12).line_height(px(18.0)).text_color(if faint { p.ink_3 } else { p.ink_2 }).child(text).into_any_element()
        };
        match &sheet.stage {
            Stage::Choose if sheet.profiles.is_empty() => {
                body.push(line("Chrome isn’t on this Mac, or has no profile yet.".into(), false));
                let close = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.close_chrome_import(cx));
                actions.push(button("chrome-import-ok", "Close").on_click(move |e, w, cx| close(e, w, cx)).into_any_element());
            }
            Stage::Choose => {
                body.push(line("Baaz reads a copy of Chrome’s files; Chrome is never changed.".into(), false));
                // The profile: a row per profile when there are several.
                if sheet.profiles.len() > 1 {
                    let mut list = v_flex().mx(px(PAD_X)).mt(px(10.0)).rounded(px(8.0)).border_1().border_color(p.line).overflow_hidden();
                    for (i, profile) in sheet.profiles.iter().enumerate() {
                        let pick = cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                            if let Some(sheet) = this.chrome_import.as_mut() {
                                sheet.selected = i;
                            }
                            this.count_chrome_profile(cx);
                            cx.notify();
                        });
                        let on = i == sheet.selected;
                        list = list.child(
                            h_flex()
                                .id(SharedString::from(format!("chrome-profile-{i}")))
                                .h(px(32.0))
                                .px(px(12.0))
                                .gap(px(8.0))
                                .items_center()
                                .cursor_pointer()
                                .when(on, |d| d.bg(p.surface_3))
                                .role(gpui::Role::RadioButton)
                                .aria_label(SharedString::from(format!("Chrome profile {}", profile.name)))
                                .on_click(pick)
                                .child(div().flex_1().text_px(scale::FS_13).text_color(p.ink).child(profile.name.clone()))
                                .when(on, |d| d.child(aui_icons::icon(IconName::Check).size(px(12.0)).color(p.ink))),
                        );
                    }
                    body.push(list.into_any_element());
                } else {
                    body.push(line(format!("Profile: {}", sheet.profiles[0].name), true));
                }
                let counts = sheet.counts.clone();
                let word = |n: Option<u64>| n.map(|n| crate::import_ui::thousands(n as usize)).unwrap_or_else(|| "…".into());
                let tick = |id: &'static str, label: &'static str, detail: &'static str, on: bool, n: Option<u64>, which: u8| {
                    let flip = cx.listener(move |this: &mut Self, intent: &CheckboxIntent, _, cx| {
                        let CheckboxIntent::Set(state) = intent;
                        if let Some(sheet) = this.chrome_import.as_mut() {
                            if which == 0 {
                                sheet.bookmarks = state.checked();
                            } else {
                                sheet.history = state.checked();
                            }
                        }
                        cx.notify();
                    });
                    h_flex()
                        .id(id)
                        .px(px(12.0))
                        .py(px(8.0))
                        .gap(px(10.0))
                        .items_start()
                        .child(
                            div().pt(px(2.0)).child(
                                checkbox(SharedString::from(format!("{id}-box")), if on { CheckboxState::On } else { CheckboxState::Off }, false)
                                    .accessibility_label(SharedString::from(format!("{label}, {}", word(n))))
                                    .on_intent(move |intent, w, cx| flip(&intent, w, cx)),
                            ),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w(px(0.0))
                                .child(div().text_px(scale::FS_13).text_color(p.ink).child(label))
                                .child(div().text_px(scale::FS_12).line_height(px(17.0)).text_color(p.ink_3).child(detail)),
                        )
                        .child(div().flex_none().text_px(scale::FS_13).text_color(p.ink_2).child(word(n)))
                        .into_any_element()
                };
                let rows = v_flex()
                    .mx(px(PAD_X))
                    .mt(px(10.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(p.line)
                    .child(tick("chrome-import-bookmarks", "Bookmarks", "Into a folder “Imported from Chrome”, folders kept.", sheet.bookmarks, counts.as_ref().map(|c| c.bookmarks), 0))
                    .child(div().h(px(1.0)).bg(p.line))
                    .child(tick(
                        "chrome-import-history",
                        "History",
                        "The last 90 days. Page words aren’t in Chrome’s history, so titles and addresses search until you visit again.",
                        sheet.history,
                        counts.as_ref().map(|c| c.history),
                        1,
                    ));
                body.push(rows.into_any_element());
                body.push(line("Passwords and sign-ins are not imported: Chrome keeps them to itself. You sign in to each site once.".into(), true));
                let cancel = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.close_chrome_import(cx));
                let go = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.run_chrome_import(cx));
                let ready = counts.is_some() && (sheet.bookmarks || sheet.history);
                actions.push(button("chrome-import-cancel", "Cancel").on_click(move |e, w, cx| cancel(e, w, cx)).into_any_element());
                actions.push(button("chrome-import-go", "Import").primary().disabled(!ready).on_click(move |e, w, cx| go(e, w, cx)).into_any_element());
            }
            Stage::Done(done) => {
                body.push(
                    div()
                        .px(px(PAD_X))
                        .pt(px(16.0))
                        .text_px(scale::FS_13)
                        .line_height(px(20.0))
                        .text_color(p.ink)
                        .child(format!(
                            "Imported {} bookmarks and {} pages of history.",
                            crate::import_ui::thousands(done.bookmarks as usize),
                            crate::import_ui::thousands(done.history as usize)
                        ))
                        .into_any_element(),
                );
                // Say where the bookmarks went only when some came in.
                let note = if done.bookmarks > 0 {
                    "Bookmarks are in “Imported from Chrome”. Find any of them in the Field (⌘L). Pages you already had are not counted twice."
                } else {
                    "No new bookmarks: this profile had none Baaz did not already have. Pages you already had are not counted twice."
                };
                body.push(line(note.into(), false));
                let undo = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.undo_chrome_import(cx));
                let ok = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.close_chrome_import(cx));
                actions.push(button("chrome-import-undo", "Undo import").ghost().on_click(move |e, w, cx| undo(e, w, cx)).into_any_element());
                actions.push(div().flex_1().into_any_element());
                actions.push(button("chrome-import-done", "Done").primary().on_click(move |e, w, cx| ok(e, w, cx)).into_any_element());
            }
        }
        let card = v_flex()
            .id("chrome-import-sheet")
            .w(px(SHEET_W))
            .max_w_full()
            .rounded(px(12.0))
            .bg(p.surface_2)
            .shadow(p.floating_shadow())
            .overflow_hidden()
            .text_color(p.ink)
            .role(gpui::Role::Dialog)
            .aria_label(title)
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(head)
            .children(body)
            .child(h_flex().w_full().items_center().gap(px(8.0)).px(px(PAD_X)).pt(px(14.0)).pb(px(16.0)).child(div().flex_1()).children(actions));
        Some(
            div()
                .absolute()
                .inset_0()
                .capture_key_down(move |event, window, cx| escape(event, window, cx))
                .child(aui::screens::sheet_scrim("chrome-import-scrim", card, None))
                .into_any_element(),
        )
    }
}
