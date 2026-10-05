//! B2a-H5 Browse chrome: the find strip, the permission strip, downloads,
//! popups-as-tabs and the crashed-page helpers.
//!
//! The rendering lives in `app.rs` (rows above the centre page, popovers
//! over it); this module owns every decision behind it, with no gpui types
//! except where a test drives a real [`WebviewState`](aui_webview::WebviewState)
//! on the scripted backend. Two notes the gate cannot prove and relay must:
//! the library reports no load-error event (only `Crashed`), so Baaz-drawn
//! error pages render from the crashed state alone; and stock CEF exposes
//! no extension list, so the extensions popover is a fixed message.

use std::collections::{HashMap, HashSet};

use aui_webview::{DownloadState, DownloadStatus, PermissionDecision, PermissionKind};

use super::tabs::{Owner, Tab, TabId, Tabs};

/// Live search starts this long after the last keystroke.
pub(crate) const FIND_DEBOUNCE_MS: u64 = 100;

/// One tab's find strip: open state, the typed query and the case toggle.
/// Per tab: switching tabs hides the strip, returning restores the query.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FindStrip {
    /// Whether the strip stands open on this tab.
    pub open: bool,
    /// The typed query; kept while hidden so returning restores it.
    pub query: String,
    /// Whether the search matches case.
    pub match_case: bool,
}

impl FindStrip {
    /// Open the strip; answers whether the backend must start a search
    /// (only when a query is already stored, e.g. reopening).
    pub(crate) fn open(&mut self) -> bool {
        let was = self.open;
        self.open = true;
        !was && !self.query.is_empty()
    }

    /// Type into the strip; answers whether the backend must search (the
    /// query changed and is non-empty). Clearing the field stops nothing:
    /// the highlight stays until the strip closes.
    pub(crate) fn set_query(&mut self, query: &str) -> bool {
        if self.query == query {
            return false;
        }
        self.query = query.to_owned();
        !self.query.is_empty()
    }

    /// Close the strip; answers whether the backend must stop the search
    /// (only when the strip stood open).
    pub(crate) fn close(&mut self) -> bool {
        let was = self.open;
        self.open = false;
        was
    }

    /// Flip the case toggle; answers whether the backend must re-search
    /// (only when open on a non-empty query).
    pub(crate) fn toggle_case(&mut self) -> bool {
        self.match_case = !self.match_case;
        self.open && !self.query.is_empty()
    }
}

/// Every tab's find strip, keyed by tab. The shown tab alone renders its
/// strip; the rest persist silently until their tab returns.
#[derive(Clone, Debug, Default)]
pub(crate) struct FindStrips {
    strips: HashMap<TabId, FindStrip>,
}

impl FindStrips {
    /// The tab's strip, opening its entry on first use.
    pub(crate) fn for_tab(&mut self, id: TabId) -> &mut FindStrip {
        self.strips.entry(id).or_default()
    }

    /// The strip to render for the shown tab, if it stands open.
    pub(crate) fn visible(&self, shown: Option<TabId>) -> Option<(TabId, &FindStrip)> {
        let id = shown?;
        let strip = self.strips.get(&id)?;
        strip.open.then_some((id, strip))
    }

    /// Forget a closed tab's strip.
    pub(crate) fn remove(&mut self, id: TabId) {
        self.strips.remove(&id);
    }
}

/// The find strip's count: "3 / 12".
pub(crate) fn find_count_text(active: u32, total: u32) -> String {
    format!("{active} / {total}")
}

/// What the permission strip names for one asked kind.
pub(crate) fn permission_capability(kind: &PermissionKind) -> &'static str {
    match kind {
        PermissionKind::Camera => "use your camera",
        PermissionKind::Microphone => "use your microphone",
        PermissionKind::Geolocation => "know your location",
        PermissionKind::Notifications => "show notifications",
        PermissionKind::Clipboard => "read your clipboard",
        PermissionKind::Midi => "use your MIDI devices",
        PermissionKind::Other(_) => "access a connected device",
    }
}

/// The permission strip's sentence: "<origin> wants to <capabilities>".
/// Several kinds join with commas and a final "and".
pub(crate) fn permission_sentence(origin: &str, kinds: &[PermissionKind]) -> String {
    let capabilities: Vec<&str> = kinds.iter().map(permission_capability).collect();
    let want = match capabilities.as_slice() {
        [] => "use a site feature".to_owned(),
        [only] => (*only).to_owned(),
        [first, second] => format!("{first} and {second}"),
        _ => {
            let (last, rest) = capabilities.split_at(capabilities.len() - 1);
            format!("{} and {}", rest.join(", "), last[0])
        }
    };
    format!("{origin} wants to {want}")
}

/// Origins the person trusted "while visiting": in memory, this run only.
/// Persistence is B2b.
#[derive(Clone, Debug, Default)]
pub(crate) struct VisitAllows {
    origins: HashSet<String>,
}

impl VisitAllows {
    /// Trust `origin` for the rest of the run.
    pub(crate) fn remember(&mut self, origin: &str) {
        self.origins.insert(origin.to_owned());
    }

    /// Whether `origin` was trusted.
    pub(crate) fn allows(&self, origin: &str) -> bool {
        self.origins.contains(origin)
    }
}

/// The host's answer for a pending request: auto-allow trusted origins,
/// no answer otherwise (the strip asks the person).
pub(crate) fn auto_answer(origin: &str, allows: &VisitAllows) -> Option<PermissionDecision> {
    allows.allows(origin).then_some(PermissionDecision::Allow)
}

/// Human bytes: "210 KB", "4.2 MB", "9.8 MB", "1.0 GB". One decimal from
/// KB up, bytes bare.
pub(crate) fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let value = bytes as f64;
    if value < KB {
        format!("{bytes} B")
    } else if value < MB {
        format!("{:.1} KB", value / KB)
    } else if value < GB {
        format!("{:.1} MB", value / MB)
    } else {
        format!("{:.1} GB", value / GB)
    }
}

/// Seconds left at the average rate so far, if a total is known and bytes
/// are moving.
pub(crate) fn download_eta_secs(received: u64, total: Option<u64>, elapsed_secs: u64) -> Option<u64> {
    let total = total?;
    if total <= received || received == 0 || elapsed_secs == 0 {
        return None;
    }
    let remaining = total - received;
    Some(remaining * elapsed_secs / received)
}

/// One download's popover line: "4.2 of 9.8 MB · 6 s left" while active
/// with a known total, "Done · 210 KB" when landed.
pub(crate) fn download_line(state: &DownloadState, elapsed_secs: u64) -> String {
    match state.state {
        DownloadStatus::Downloading => match state.total {
            Some(_) => match download_eta_secs(state.received, state.total, elapsed_secs) {
                Some(eta) => format!(
                    "{} of {} · {eta} s left",
                    format_bytes(state.received),
                    format_bytes(state.total.unwrap_or(0))
                ),
                None => format!("{} · downloading", format_bytes(state.received)),
            },
            None => format!("{} · downloading", format_bytes(state.received)),
        },
        DownloadStatus::Complete => format!("Done · {}", format_bytes(state.received)),
        DownloadStatus::Cancelled => format!("Cancelled · {}", format_bytes(state.received)),
        DownloadStatus::Failed => format!("Failed · {}", format_bytes(state.received)),
    }
}

/// How many downloads are still moving.
pub(crate) fn downloads_active_count(states: &[DownloadState]) -> usize {
    states.iter().filter(|state| state.state == DownloadStatus::Downloading).count()
}

/// The toolbar button's label: "Downloads, 1 active".
pub(crate) fn downloads_label(states: &[DownloadState]) -> String {
    format!("Downloads, {} active", downloads_active_count(states))
}

/// The agent credit for a download on a session-owned tab: "saved by Fix
/// ledger chart". Person tabs carry none.
pub(crate) fn download_owner_label(owner: &Owner, session_name: Option<&str>) -> Option<String> {
    match owner {
        Owner::Person => None,
        Owner::Session(_) => session_name.map(|name| format!("saved by {name}")),
    }
}

/// The tab a popup opens as: right after its opener, with the opener's
/// owner and space — session-owned openers create session-owned popups.
/// Returns the new tab's id, selecting it only for foreground popups.
pub(crate) fn place_popup_tab(tabs: &mut Tabs, opener: TabId, url: &str, foreground: bool) -> Option<TabId> {
    let opener_tab = tabs.get(opener)?.clone();
    let active = tabs.active_of(&opener_tab.owner, opener_tab.space.as_deref());
    let id = tabs.open(opener_tab.space.clone(), opener_tab.owner.clone(), url, Some(opener));
    if !foreground {
        restore_active(tabs, &opener_tab, active);
    }
    Some(id)
}

/// Keep a background popup from stealing the owner's active tab:
/// [`Tabs::open`] always selects, so put the opener's pointer back.
fn restore_active(tabs: &mut Tabs, opener_tab: &Tab, active: Option<TabId>) {
    if let Some(previous) = active {
        let key = super::tabs::active_key(&opener_tab.owner, opener_tab.space.as_deref());
        tabs.active.insert(key, previous);
    }
}

/// The host part of `url` for the error page ("Can't reach <host>"),
/// falling back to the whole URL when no host parses out.
pub(crate) fn error_host(url: &str) -> &str {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let host = after_scheme.split(['/','?','#']).next().unwrap_or(after_scheme);
    if host.is_empty() { url } else { host }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Open, type, move, close: the strip tells the caller exactly when
    /// the backend must run.
    #[test]
    fn find_strip_state_machine_opens_types_and_closes() {
        let mut strip = FindStrip::default();
        assert!(!strip.open, "a fresh strip starts closed");
        assert!(!strip.open(), "opening with no query starts no search");
        assert!(strip.open, "the strip stands open");
        assert!(strip.set_query("price"), "typing a query searches");
        assert!(!strip.set_query("price"), "retyping the same query searches nothing");
        assert!(strip.set_query("prices"), "changing the query re-searches");
        assert!(!strip.set_query(""), "clearing the field stops nothing by itself");
        assert!(strip.toggle_case(), "flipping case re-searches the kept query");
        assert!(strip.close(), "closing an open strip stops the search");
        assert!(!strip.close(), "closing twice stops once");
        assert_eq!(strip.query, "", "the cleared query stays cleared");
    }

    #[test]
    fn find_strips_are_per_tab_and_restore_on_return() {
        let mut tabs = Tabs::default();
        let first = tabs.open(None, Owner::Person, "https://a.example/", None);
        let second = tabs.open(None, Owner::Person, "https://b.example/", None);
        let mut strips = FindStrips::default();
        assert!(strips.visible(Some(first)).is_none(), "no strip until opened");
        strips.for_tab(first).open();
        strips.for_tab(first).set_query("ledger");
        assert!(strips.visible(Some(second)).is_none(), "switching tabs hides the strip");
        let (id, strip) = strips.visible(Some(first)).expect("returning restores the query");
        assert_eq!(id, first);
        assert_eq!(strip.query, "ledger");
        assert!(strips.for_tab(first).close(), "closing an open strip stops the search");
        assert!(!strips.for_tab(first).close(), "closing twice stops once");
        strips.remove(second);
    }

    #[test]
    fn find_count_reads_active_over_total() {
        assert_eq!(find_count_text(3, 12), "3 / 12");
        assert_eq!(find_count_text(0, 0), "0 / 0");
    }

    #[test]
    fn permission_sentence_names_every_kind() {
        assert_eq!(
            permission_sentence("https://cam.example/", &[PermissionKind::Camera]),
            "https://cam.example/ wants to use your camera"
        );
        assert_eq!(
            permission_sentence("https://mic.example/", &[PermissionKind::Microphone]),
            "https://mic.example/ wants to use your microphone"
        );
        assert_eq!(
            permission_sentence("https://geo.example/", &[PermissionKind::Geolocation]),
            "https://geo.example/ wants to know your location"
        );
        assert_eq!(
            permission_sentence("https://note.example/", &[PermissionKind::Notifications]),
            "https://note.example/ wants to show notifications"
        );
        assert_eq!(
            permission_sentence("https://clip.example/", &[PermissionKind::Clipboard]),
            "https://clip.example/ wants to read your clipboard"
        );
        assert_eq!(
            permission_sentence("https://midi.example/", &[PermissionKind::Midi]),
            "https://midi.example/ wants to use your MIDI devices"
        );
        assert_eq!(
            permission_sentence("https://usb.example/", &[PermissionKind::Other(64)]),
            "https://usb.example/ wants to access a connected device"
        );
    }

    #[test]
    fn permission_sentence_joins_combined_kinds() {
        assert_eq!(
            permission_sentence("https://call.example/", &[PermissionKind::Camera, PermissionKind::Microphone]),
            "https://call.example/ wants to use your camera and use your microphone"
        );
        assert_eq!(
            permission_sentence(
                "https://all.example/",
                &[PermissionKind::Camera, PermissionKind::Microphone, PermissionKind::Geolocation]
            ),
            "https://all.example/ wants to use your camera, use your microphone and know your location"
        );
    }

    #[test]
    fn allow_while_visiting_auto_answers_repeats() {
        let mut allows = VisitAllows::default();
        assert_eq!(auto_answer("https://cam.example/", &allows), None, "strangers ask");
        allows.remember("https://cam.example/");
        assert_eq!(
            auto_answer("https://cam.example/", &allows),
            Some(PermissionDecision::Allow),
            "trusted origins auto-allow"
        );
        assert_eq!(auto_answer("https://other.example/", &allows), None, "trust is per origin");
    }

    #[test]
    fn download_lines_read_size_and_state() {
        let active = DownloadState {
            id: 1,
            url: "https://cdn.example/big.zip".to_owned(),
            file_name: "big.zip".to_owned(),
            path: "/Users/test/Downloads/big.zip".to_owned(),
            received: 4_400_000,
            total: Some(10_200_000),
            state: DownloadStatus::Downloading,
        };
        let line = download_line(&active, 8);
        assert!(line.starts_with("4.2 of 9.7 MB"), "sizes humanise: {line}");
        assert!(line.ends_with("s left"), "active downloads estimate: {line}");
        let done = DownloadState { state: DownloadStatus::Complete, received: 215_040, ..active.clone() };
        assert_eq!(download_line(&done, 0), "Done · 210.0 KB");
        let unknown = DownloadState { total: None, ..active.clone() };
        assert_eq!(download_line(&unknown, 0), "4.2 MB · downloading");
        assert_eq!(downloads_active_count(&[active.clone(), done.clone()]), 1);
        assert_eq!(downloads_label(&[active, done]), "Downloads, 1 active");
    }

    #[test]
    fn download_owner_label_credits_session_downloads() {
        assert_eq!(download_owner_label(&Owner::Person, Some("Fix ledger chart")), None);
        assert_eq!(
            download_owner_label(&Owner::Session("s1".to_owned()), Some("Fix ledger chart")),
            Some("saved by Fix ledger chart".to_owned())
        );
    }

    #[test]
    fn popup_tabs_land_after_the_opener_with_its_owner() {
        let mut tabs = Tabs::default();
        let opener = tabs.open(Some("proj".to_owned()), Owner::Session("s1".to_owned()), "https://app.example/", None);
        let next = tabs.open(Some("proj".to_owned()), Owner::Person, "https://other.example/", None);
        let popup = place_popup_tab(&mut tabs, opener, "https://popup.example/", true).expect("popup opens");
        let order: Vec<TabId> = tabs.tabs.iter().map(|tab| tab.id).collect();
        assert_eq!(order, vec![opener, popup, next], "the popup sits right after its opener");
        let tab = tabs.get(popup).expect("popup exists");
        assert_eq!(tab.owner, Owner::Session("s1".to_owned()), "session openers make session popups");
        assert_eq!(tab.space.as_deref(), Some("proj"));
        assert_eq!(tabs.active_of(&Owner::Session("s1".to_owned()), None), Some(popup), "foreground selects");
        let background = place_popup_tab(&mut tabs, opener, "https://quiet.example/", false).expect("opens");
        assert_eq!(
            tabs.active_of(&Owner::Session("s1".to_owned()), None),
            Some(popup),
            "background popups keep the opener active (still {background:?})"
        );
    }

    #[test]
    fn error_host_names_the_host() {
        assert_eq!(error_host("https://example.com/a/b?x=1"), "example.com");
        assert_eq!(error_host("about:blank"), "about:blank");
    }

    use aui_webview::{FakeWebBackend, WebviewState};
    use gpui::AppContext as _;

    /// A scripted page wrapped as a live state: events scripted before the
    /// wrap drain on the poll timer.
    fn scripted_state(
        cx: &mut gpui::TestAppContext,
        script: impl FnOnce(&mut FakeWebBackend),
    ) -> gpui::Entity<WebviewState> {
        let mut backend = FakeWebBackend::new();
        script(&mut backend);
        cx.new(|cx| WebviewState::new(Box::new(backend), cx))
    }

    /// Find results land on the state and closing the strip clears them:
    /// `stop_find` drops the highlight synchronously.
    #[gpui::test]
    fn find_results_arrive_and_close_clears(cx: &mut gpui::TestAppContext) {
        let state = scripted_state(cx, |backend| backend.script_find("price", 3, 12, true));
        cx.run_until_parked();
        let found = state.update(cx, |state, _| state.find_state());
        let found = found.expect("the scripted result lands");
        assert_eq!((found.query.as_str(), found.active, found.total), ("price", 3, 12));
        assert_eq!(find_count_text(found.active, found.total), "3 / 12");
        let mut strip = FindStrip { open: true, query: "price".to_owned(), match_case: false };
        assert!(strip.close(), "closing stops the search");
        state.update(cx, |state, _| state.stop_find());
        assert!(
            state.update(cx, |state, _| state.find_state()).is_none(),
            "closing the strip clears the highlight"
        );
    }

    /// Answering a permission drops its strip; a page dismissal does too.
    #[gpui::test]
    fn permission_answer_and_dismissal_clear_the_strip(cx: &mut gpui::TestAppContext) {
        use aui_webview::PermissionDecision;
        let state = scripted_state(cx, |backend| {
            backend.script_permission(7, "https://cam.example/", vec![PermissionKind::Camera])
        });
        cx.run_until_parked();
        let pending = state.update(cx, |state, _| state.permissions().to_vec());
        assert_eq!(pending.len(), 1, "the ask waits for an answer");
        assert_eq!(
            permission_sentence(&pending[0].origin, &pending[0].kinds),
            "https://cam.example/ wants to use your camera"
        );
        state.update(cx, |state, cx| state.answer_permission(7, PermissionDecision::Deny, cx));
        assert!(state.update(cx, |state, _| state.permissions().is_empty()), "denying clears the ask");
        let dismissed = scripted_state(cx, |backend| {
            backend.script_permission(9, "https://geo.example/", vec![PermissionKind::Geolocation]);
            backend.script_permission_dismissed(9);
        });
        cx.run_until_parked();
        assert!(
            dismissed.update(cx, |state, _| state.permissions().is_empty()),
            "a page dismissal drops the strip with no answer"
        );
    }

    /// Downloads list on the state with their sizes; cancelling is offered
    /// while active (the end arrives as an event).
    #[gpui::test]
    fn downloads_list_with_sizes(cx: &mut gpui::TestAppContext) {
        use aui_webview::{DownloadState as LibraryDownload, DownloadStatus as LibraryStatus};
        let state = scripted_state(cx, |backend| {
            backend.script_download(LibraryDownload {
                id: 3,
                url: "https://cdn.example/big.zip".to_owned(),
                file_name: "big.zip".to_owned(),
                path: "/Users/test/Downloads/big.zip".to_owned(),
                received: 215_040,
                total: Some(1_048_576),
                state: LibraryStatus::Downloading,
            })
        });
        cx.run_until_parked();
        let listed = state.update(cx, |state, _| state.downloads().to_vec());
        assert_eq!(listed.len(), 1, "the download lists");
        assert_eq!(download_line(&listed[0], 4), "210.0 KB of 1.0 MB · 15 s left");
    }

    /// A crash shows the Baaz page and the next navigation clears it.
    #[gpui::test]
    fn crashed_shows_until_the_next_navigation(cx: &mut gpui::TestAppContext) {
        let state = scripted_state(cx, |backend| backend.script_crashed("the page crashed (-1)"));
        cx.run_until_parked();
        assert!(
            state.update(cx, |state, _| state.crashed()).is_some(),
            "the crash shows the Baaz page"
        );
        state.update(cx, |state, _| state.navigate("https://example.com/"));
        cx.run_until_parked();
        assert!(
            state.update(cx, |state, _| state.crashed()).is_none(),
            "the next navigation clears the crash"
        );
    }

    /// Popup and open-request backends drain into tabs after the opener.
    #[gpui::test]
    fn popup_backends_become_tabs_after_the_opener(cx: &mut gpui::TestAppContext) {
        let state = scripted_state(cx, |backend| {
            backend.script_popup("https://popup.example/", true);
            backend.script_open_request("https://example.com/menu", false);
        });
        let mut tabs = Tabs::default();
        let opener = tabs.open(None, Owner::Person, "https://app.example/", None);
        let (popups, opens) = state.update(cx, |state, _| {
            (state.take_popup_backends(), state.take_open_requests())
        });
        assert_eq!(popups.len(), 1, "the popup drains");
        assert_eq!(opens, vec![("https://example.com/menu".to_owned(), false)]);
        let popup = place_popup_tab(&mut tabs, opener, &popups[0].0, popups[0].1).expect("popup tabs");
        assert_eq!(tabs.get(popup).expect("popup").url, "https://popup.example/");
        let order: Vec<TabId> = tabs.tabs.iter().map(|tab| tab.id).collect();
        assert_eq!(order[1], popup, "the popup sits right after its opener");
    }
}
