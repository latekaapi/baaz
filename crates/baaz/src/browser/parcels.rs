//! What leaves a page or the right pane for a session's composer (docs/82
//! 8.2): a page's screenshot, its notes (Annotate's Send), a browser chat's
//! hand-off, a quote from the pane, a terminal block's Ask.
//!
//! No path writes into a composer the person cannot see. Where the active
//! session's composer is on screen — Sessions, a session open, no full page
//! over it — each goes there as it always did. Anywhere
//! else (Browse, Settings, no session) the picker asks "Add to which
//! session?" ([`Harness::open_attach_picker`]); the content goes to the
//! picked session only, keyed by its id and never by `active`, and a toast
//! says where it went, with Open. A cancelled picker attaches nothing
//! anywhere: the parcel rides the picker's own overlay entry and is dropped
//! with it.
//!
//! A picked session whose view is not loaded keeps its parcel here
//! ([`BrowserView::parcels`]) until its view lands (`Harness::activate`); a
//! new session's parcel waits untargeted until its open names the id (the
//! keying the old Ask draft used, B2a-H4d).

use super::*;
use crate::app::browser_handoff::HandoffTarget;
use crate::layout::LayoutGlobal as _;

/// Content bound for a session's composer.
pub(crate) enum Parcel {
    /// Words for the draft (a browser chat's hand-off), appended on a new
    /// line under what is already typed.
    Text(String),
    /// A page screenshot: the chip's name and its PNG bytes.
    Screenshot { name: String, bytes: Vec<u8> },
    /// A page's notes, as Annotate's Send carries them.
    Annotations(Box<NotesParcel>),
    /// Lines quoted from the right pane (a file, a diff, a check, a hook's
    /// output): the composer's quote chip.
    Quote { text: String, lang: String, lines: usize, location: Option<crate::session::QuoteLocation> },
    /// A terminal block's Ask: its command and output, quoted in the draft.
    TerminalOutput { command: String, output: String },
}

/// Annotate's Send, held until it lands: the parts the batch is built from
/// once the target session (whose sent folder keeps the picture) is known.
pub(crate) struct NotesParcel {
    /// The pins, in the order they were dropped.
    pub(crate) annotations: Vec<Annotation>,
    /// The annotated page, when the backend produced one.
    pub(crate) screenshot: Option<Vec<u8>>,
    /// The page they were taken on.
    pub(crate) url: String,
    /// Where each pin sits on the page, in annotation order.
    pub(crate) pins: Vec<Option<(f32, f32)>>,
    /// The page's size in the same points.
    pub(crate) page_size: (f32, f32),
    /// The page the notes were made on: its panel empties once they land
    /// (a cancelled pick leaves them there to send again).
    pub(crate) page: Option<gpui::WeakEntity<WebviewState>>,
}

impl Parcel {
    /// What the toast calls it: "Screenshot added to …".
    pub(crate) fn noun(&self) -> &'static str {
        match self {
            Parcel::Text(_) => "Draft",
            Parcel::Screenshot { .. } => "Screenshot",
            Parcel::Annotations(_) => "Notes",
            Parcel::Quote { .. } => "Quote",
            Parcel::TerminalOutput { .. } => "Terminal output",
        }
    }
}

/// A parcel waiting for its session's view: `target` is the session id, or
/// `None` while a new session's open is in flight (an unrelated landing
/// never takes it, and a refused open drops it). `announce` is the toast a
/// new session's landing raises, its id known only then.
pub(crate) struct PendingParcel {
    pub(crate) target: Option<String>,
    pub(crate) parcel: Parcel,
    pub(crate) announce: Option<String>,
}

impl BrowserView {
    /// Key the untargeted parcels to `session`: the id `new_session_in`
    /// created (or the draft id it reused). Called on every path that opens
    /// the new session, before its view lands — so `activate` hands them to
    /// this session only.
    pub(crate) fn key_pending_parcels(&mut self, session: &str) {
        for pending in self.parcels.iter_mut().filter(|pending| pending.target.is_none()) {
            pending.target = Some(session.to_owned());
        }
    }

    /// Drop the untargeted parcels: every early return in `new_session_in`
    /// (a switch already pending, no params, a failed or never-landing
    /// provider open) drops them, so no unrelated landing ever takes them.
    /// Parcels keyed to an existing session stay.
    pub(crate) fn drop_untargeted_parcels(&mut self) {
        self.parcels.retain(|pending| pending.target.is_some());
    }

    /// A provisional id became the provider's own: its parcels follow.
    pub(crate) fn rekey_parcels(&mut self, old: &str, new: &str) {
        for pending in self.parcels.iter_mut().filter(|pending| pending.target.as_deref() == Some(old)) {
            pending.target = Some(new.to_owned());
        }
    }

    /// Take the parcels keyed to `session`, oldest first.
    fn take_parcels_for(&mut self, session: &str) -> Vec<PendingParcel> {
        let (mine, rest) = std::mem::take(&mut self.parcels).into_iter().partition(|pending| pending.target.as_deref() == Some(session));
        self.parcels = rest;
        mine
    }
}

impl Harness {
    /// Whether the active session's composer is on screen: Sessions, a
    /// session open, and no full page (Settings, Skills, the connect
    /// screen) standing over it.
    pub(crate) fn composer_on_screen(&self, cx: &gpui::App) -> bool {
        crate::layout::Layout::get(cx).mode == crate::layout::Mode::Sessions
            && self.active.is_some()
            && !self.settings_open(cx)
            && !self.skills_open(cx)
            && !self.connect_open(cx)
    }

    /// Send `parcel` toward a composer: the active session's when it is on
    /// screen, else the "Add to which session?" picker.
    pub(crate) fn send_parcel(&mut self, parcel: Parcel, window: &mut Window, cx: &mut Context<Self>) {
        match self.active.clone().filter(|_| self.composer_on_screen(cx)) {
            Some(view) => self.apply_parcel(&view, parcel, window, cx),
            None => self.open_attach_picker(parcel, cx),
        }
    }

    /// The picker's pick: the parcel its entry held goes to `target`. No
    /// picker open (it closed already) does nothing.
    pub(crate) fn attach_picked(&mut self, target: HandoffTarget, window: &mut Window, cx: &mut Context<Self>) {
        let entry = self.overlays.update(cx, |o, cx| o.close(crate::overlay_stack::LayerKind::AttachPicker, cx));
        let Some(crate::overlay_stack::Layer::AttachPicker(parcel)) = entry.map(|entry| entry.layer) else { return };
        self.deliver_parcel(target, *parcel, window, cx);
    }

    /// Deliver `parcel` to `target` and say where it went. An existing
    /// session takes it now when its view is loaded (open or parked), else
    /// when its view lands; the person stays where they are, and the
    /// toast's Open takes them there. A new session opens in its project —
    /// there is no new session without opening one — and its parcel lands
    /// with it, the toast following once its id is known. Answers the
    /// session it went to, when that is known now (a new session's open
    /// may still be in flight, or refused).
    pub(crate) fn deliver_parcel(&mut self, target: HandoffTarget, parcel: Parcel, window: &mut Window, cx: &mut Context<Self>) -> Option<String> {
        let noun = parcel.noun();
        match target {
            HandoffTarget::Session(id) => {
                // A hand-off chain opens as its head: the parcel waits there.
                let id = self.chain_head(&id, cx);
                let title = self.parcel_session_title(&id, cx);
                match self.find_view(&id, cx) {
                    Some(view) => self.apply_parcel(&view, parcel, window, cx),
                    None => self.browser.update(cx, |browser, _| browser.parcels.push(PendingParcel { target: Some(id.clone()), parcel, announce: None })),
                }
                let headline = if noun == "Draft" { format!("Continued in {title}") } else { format!("{noun} added to {title}") };
                self.overlays.update(cx, |o, cx| o.toast_open(headline, "Waiting in its composer, unsent.", &id, cx));
                Some(id)
            }
            HandoffTarget::Project(project) => {
                let name = self.session_list.read(cx).projects.find(&project).map(|p| p.name.clone()).unwrap_or_default();
                let headline = if noun == "Draft" { format!("Sent to a new session in {name}") } else { format!("{noun} added to a new session in {name}") };
                let before = self.active_id(cx);
                self.browser.update(cx, |browser, _| browser.parcels.push(PendingParcel { target: None, parcel, announce: Some(headline.clone()) }));
                self.new_session_in(Some(project), window, cx);
                // Still waiting: keyed (its view not landed yet) or in
                // flight. Gone: landed in the session now open, or dropped
                // with a refused open (nothing new is open then).
                let waiting = self.browser.read(cx).parcels.iter().find(|p| p.announce.as_deref() == Some(headline.as_str())).map(|p| p.target.clone());
                match waiting {
                    Some(target) => target,
                    None => self.active_id(cx).filter(|now| Some(now) != before.as_ref()),
                }
            }
        }
    }

    /// The session's title as the sidebar shows it.
    fn parcel_session_title(&self, id: &str, cx: &gpui::App) -> String {
        self.session_list
            .read(cx)
            .sessions
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.label.clone())
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| "the session".to_owned())
    }

    /// `session_id`'s view landed: the parcels keyed to it go into its
    /// composer, unsent, and a new session's landing raises its toast.
    pub(crate) fn land_parcels(&mut self, session_id: &str, view: &Entity<crate::session::SessionView>, window: &mut Window, cx: &mut Context<Self>) {
        let landed = self.browser.update(cx, |browser, _| browser.take_parcels_for(session_id));
        for pending in landed {
            self.apply_parcel(view, pending.parcel, window, cx);
            if let Some(headline) = pending.announce {
                self.overlays.update(cx, |o, cx| o.toast_open(headline, "Waiting in its composer, unsent.", session_id, cx));
            }
        }
    }

    /// Put `parcel` into `view`'s composer: never sent.
    pub(crate) fn apply_parcel(&mut self, view: &Entity<crate::session::SessionView>, parcel: Parcel, window: &mut Window, cx: &mut Context<Self>) {
        match parcel {
            Parcel::Text(text) => {
                let existing = view.read(cx).draft_text(cx);
                let next = toolbar::append_draft_line(&existing, &text);
                view.update(cx, |view, cx| view.set_draft(next, window, cx));
            }
            Parcel::Quote { text, lang, lines, location } => {
                // The session owns its quotes and redraws itself.
                view.update(cx, |view, cx| view.add_quote(text, None, Some((lang, lines)), location, window, cx));
            }
            Parcel::TerminalOutput { command, output } => {
                let existing = view.read(cx).draft_text(cx);
                let next = crate::terminal::intents::build_ask_draft(&command, &output, &existing);
                view.update(cx, |view, cx| view.set_draft(next, window, cx));
            }
            Parcel::Screenshot { name, bytes } => {
                let _path = write_screenshot_temp(&bytes);
                view.update(cx, |view, cx| view.attach_screenshot(name, bytes, cx));
            }
            Parcel::Annotations(notes) => {
                let NotesParcel { annotations, screenshot, url, pins, page_size, page } = *notes;
                // The screenshot still rides as an image the agent can see;
                // the batch (D4) remembers it, the pins and the notes so the
                // sent turn draws as one annotation turn.
                let session = view.read(cx).rt(cx).session_id.clone();
                let shot_path = screenshot.as_ref().and_then(|bytes| crate::sent::save_picture(&session, "png", bytes));
                if let Some(bytes) = screenshot {
                    let _path = write_screenshot_temp(&bytes);
                    view.update(cx, |view, cx| view.attach_screenshot(ANNOTATION_SHOT_NAME.to_owned(), bytes, cx));
                }
                let batch = annotation_batch(&annotations, &url, &pins, page_size, shot_path);
                let block = annotations_draft_block(&annotations, &url);
                view.update(cx, |view, cx| {
                    view.append_draft_block(block, window, cx);
                    view.runtime().clone().update(cx, |rt, cx| rt.set_annotation_draft(batch, cx));
                });
                // The batch now lives in the composer: the panel empties,
                // as the spec's "the batch becomes the annotation turn"
                // means (owner 2026-10-21: the list stayed after Send).
                // Annotate stays on.
                if let Some(page) = page.and_then(|page| page.upgrade()) {
                    page.update(cx, |state, cx| state.clear_annotations(cx));
                }
            }
        }
    }

    /// The toast's Open: that session, in Sessions, over whatever page
    /// stood (Settings, Skills).
    pub(crate) fn open_parcel_target(&mut self, session: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_open(cx) {
            self.close_settings(cx);
        }
        self.set_mode(crate::layout::Mode::Sessions, window, cx);
        if self.active_id(cx).as_deref() != Some(session.as_str()) {
            self.resume(session, window, cx);
        }
    }
}
