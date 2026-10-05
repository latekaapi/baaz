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
    /// The live-search generation: typing bumps it and arms a 100 ms
    /// timer, and only the latest generation searches.
    generation: u64,
    /// The generation each tab's timer armed: the debounce is per tab, so
    /// a switch never lets a stale tab's timer search the new one.
    armed: HashMap<TabId, u64>,
}

impl FindStrips {
    /// The tab's strip, opening its entry on first use.
    pub(crate) fn for_tab(&mut self, id: TabId) -> &mut FindStrip {
        self.strips.entry(id).or_default()
    }

    /// Arm the tab's debounce timer: answers the generation the timer
    /// must still hold when it fires.
    pub(crate) fn arm(&mut self, id: TabId) -> u64 {
        self.generation += 1;
        let generation = self.generation;
        self.armed.insert(id, generation);
        generation
    }

    /// Whether the tab's timer for `generation` may still search: only
    /// when no newer keystroke (on any tab) armed after it and the tab is
    /// still the one expecting it.
    pub(crate) fn armed_current(&self, id: TabId, generation: u64) -> bool {
        self.generation == generation && self.armed.get(&id) == Some(&generation)
    }

    /// The strip to render for the shown tab, if it stands open.
    pub(crate) fn visible(&self, shown: Option<TabId>) -> Option<(TabId, &FindStrip)> {
        let id = shown?;
        let strip = self.strips.get(&id)?;
        strip.open.then_some((id, strip))
    }

    /// Whether any tab's strip stands open: the shared find input lives
    /// while one does, so closing one tab's strip never strands the rest.
    pub(crate) fn any_open(&self) -> bool {
        self.strips.values().any(|strip| strip.open)
    }

    /// Forget a closed tab's strip.
    pub(crate) fn remove(&mut self, id: TabId) {
        self.strips.remove(&id);
    }

    /// Forget every strip whose tab is gone (closed tabs keep nothing).
    pub(crate) fn retain(&mut self, live: &HashSet<TabId>) {
        self.strips.retain(|id, _| live.contains(id));
        self.armed.retain(|id, _| live.contains(id));
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
/// One kind keeps its full capability ("use your camera", "know your
/// location"); several join as nouns ("camera and microphone",
/// "camera, microphone and location") behind one "use your".
pub(crate) fn permission_sentence(origin: &str, kinds: &[PermissionKind]) -> String {
    format!("{origin} wants to {}", permission_want(kinds))
}

/// The ask behind the strip's sentence, without the origin: "use your
/// camera", "know your location", "use your camera and microphone".
/// The strip renders the host on its own (bold), then this.
pub(crate) fn permission_want(kinds: &[PermissionKind]) -> String {
    if kinds.len() <= 1 {
        return match kinds.first() {
            None => "use a site feature".to_owned(),
            Some(only) => permission_capability(only).to_owned(),
        };
    }
    let nouns: Vec<&str> = kinds.iter().map(permission_noun).collect();
    let (rest, last) = nouns.split_at(nouns.len() - 1);
    let joined = if rest.len() == 1 {
        format!("{} and {}", rest[0], last[0])
    } else {
        format!("{} and {}", rest.join(", "), last[0])
    };
    format!("use your {joined}")
}

/// One asked kind as a noun, for the combined sentence.
fn permission_noun(kind: &PermissionKind) -> &'static str {
    match kind {
        PermissionKind::Camera => "camera",
        PermissionKind::Microphone => "microphone",
        PermissionKind::Geolocation => "location",
        PermissionKind::Notifications => "notifications",
        PermissionKind::Clipboard => "clipboard",
        PermissionKind::Midi => "MIDI devices",
        PermissionKind::Other(_) => "a connected device",
    }
}

/// Origins the person trusted "while visiting": in memory, this run only.
/// Persistence is B2b.
///
/// Keyed by (origin, kind): trusting the camera on an origin never
/// auto-allows its microphone — only the kinds granted skip the strip.
#[derive(Clone, Debug, Default)]
pub(crate) struct VisitAllows {
    grants: HashSet<(String, String)>,
}

impl VisitAllows {
    /// Trust `origin` for `kinds` for the rest of the run.
    pub(crate) fn remember(&mut self, origin: &str, kinds: &[PermissionKind]) {
        for kind in kinds {
            self.grants.insert((origin.to_owned(), permission_kind_key(kind)));
        }
    }

    /// Whether every kind in `kinds` was trusted on `origin`.
    pub(crate) fn allows_all(&self, origin: &str, kinds: &[PermissionKind]) -> bool {
        kinds.iter().all(|kind| self.grants.contains(&(origin.to_owned(), permission_kind_key(kind))))
    }

    /// Drop every grant for `origin`: the tab navigated away or closed.
    pub(crate) fn revoke_origin(&mut self, origin: &str) {
        self.grants.retain(|(grant_origin, _)| grant_origin != origin);
    }
}

/// The stable key for one asked kind: the grant table's second half.
pub(crate) fn permission_kind_key(kind: &PermissionKind) -> String {
    match kind {
        PermissionKind::Camera => "camera".to_owned(),
        PermissionKind::Microphone => "microphone".to_owned(),
        PermissionKind::Geolocation => "geolocation".to_owned(),
        PermissionKind::Notifications => "notifications".to_owned(),
        PermissionKind::Clipboard => "clipboard".to_owned(),
        PermissionKind::Midi => "midi".to_owned(),
        PermissionKind::Other(bit) => format!("other:{bit}"),
    }
}

/// The origin a permission grant is keyed by: `request.origin` must read
/// as a full origin (scheme + host + port, no path) —
/// `https://example.com`, never `https://example.com/page`. A host-only
/// report (`example.com`) borrows the scheme and port from the tab's own
/// URL, so the grant still names one origin.
pub(crate) fn normalize_origin(request_origin: &str, tab_url: &str) -> String {
    if let Some(full) = split_origin(request_origin) {
        return full;
    }
    let host = request_origin.trim().trim_end_matches('/').split('/').next().unwrap_or("").to_owned();
    if host.is_empty() {
        return request_origin.to_owned();
    }
    let (scheme, port) = split_scheme_port(tab_url);
    match scheme {
        Some(scheme) => format!("{scheme}://{host}{port}"),
        None => host,
    }
}

/// `https://example.com:8443/page` to `https://example.com:8443`, or
/// `None` when `value` carries no scheme (a bare host, empty, a path).
fn split_origin(value: &str) -> Option<String> {
    let (scheme, rest) = value.split_once("://")?;
    if scheme.is_empty() || !scheme.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.')) {
        return None;
    }
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() {
        return None;
    }
    Some(format!("{}://{}", scheme.to_ascii_lowercase(), authority))
}

/// The scheme and `:port` suffix behind a tab URL's authority, for
/// host-only permission origins to borrow.
fn split_scheme_port(tab_url: &str) -> (Option<String>, String) {
    let Some((scheme, rest)) = tab_url.split_once("://") else {
        return (None, String::new());
    };
    let authority = rest.split('/').next().unwrap_or("");
    let port = match authority.split_once(':') {
        Some((_, port)) if !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()) => {
            format!(":{port}")
        }
        _ => String::new(),
    };
    (Some(scheme.to_ascii_lowercase()), port)
}

/// The host's answer for a pending request: auto-allow only when every
/// asked kind was trusted on the (normalised) origin, no answer
/// otherwise (the strip asks the person).
pub(crate) fn auto_answer(origin: &str, kinds: &[PermissionKind], allows: &VisitAllows) -> Option<PermissionDecision> {
    allows.allows_all(origin, kinds).then_some(PermissionDecision::Allow)
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

/// `bytes` in `scale`'s unit, with [`format_bytes`]' decimals: the
/// received half of "4.2 of 9.8 MB" reads in the total's unit, never
/// repeating it.
fn format_bytes_scaled(bytes: u64, scale: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let bytes = bytes as f64;
    if scale < 1024 {
        format!("{} B", bytes as u64)
    } else if (scale as f64) < MB {
        format!("{:.1}", bytes / KB)
    } else if (scale as f64) < GB {
        format!("{:.1}", bytes / MB)
    } else {
        format!("{:.1}", bytes / GB)
    }
}

/// One download's popover line: "4.2 of 9.8 MB · 6 s left" while active
/// with a known total (the received half reads in the total's unit),
/// "Done · 210.0 KB" when landed.
pub(crate) fn download_line(state: &DownloadState, elapsed_secs: u64) -> String {
    match state.state {
        DownloadStatus::Downloading => match state.total {
            Some(total) => match download_eta_secs(state.received, state.total, elapsed_secs) {
                Some(eta) => {
                    format!("{} of {} · {eta} s left", format_bytes_scaled(state.received, total), format_bytes(total))
                }
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

/// The toolbar badge behind the downloads button: the overall percent
/// across active downloads with a known total, an indeterminate mark
/// while any active total is unknown, the tray mark when idle. The
/// percent sums only known totals and never reads past 100.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DownloadsBadge {
    /// What the ring reads (`"42%"`, `"…"`, `"↓"`).
    pub label: String,
    /// Whether any download is still moving (the accent ring).
    pub active: bool,
    /// Whether an active total is unknown (the indeterminate ring).
    pub indeterminate: bool,
}

pub(crate) fn downloads_badge(states: &[DownloadState]) -> DownloadsBadge {
    let mut active = false;
    let mut unknown = false;
    let (mut received, mut total) = (0u64, 0u64);
    for state in states {
        if state.state != DownloadStatus::Downloading {
            continue;
        }
        active = true;
        match state.total {
            Some(known) => {
                received = received.saturating_add(state.received);
                total = total.saturating_add(known);
            }
            None => unknown = true,
        }
    }
    if !active {
        return DownloadsBadge { label: "↓".to_owned(), active: false, indeterminate: false };
    }
    if unknown || total == 0 {
        return DownloadsBadge { label: "…".to_owned(), active: true, indeterminate: true };
    }
    let pct = received.checked_mul(100).and_then(|scaled| scaled.checked_div(total)).unwrap_or(100).min(100);
    DownloadsBadge { label: format!("{pct}%"), active: true, indeterminate: false }
}

/// Whether a popup (or open request) may take the centre: only the shown
/// tab's own popups may — a foreground ask from a background opener lands
/// in the background instead.
pub(crate) fn popup_foreground(requested: bool, opener: TabId, shown: Option<TabId>) -> bool {
    requested && Some(opener) == shown
}

/// Whether the downloads popover flag clears this frame: no centre tab,
/// or an emptied list that already rendered once. A flag that never
/// rendered stays, so a fresh download's first frame never closes its
/// own capture.
pub(crate) fn downloads_flag_clears(centre_shown: bool, empty: bool, rendered: bool) -> bool {
    !centre_shown || (rendered && empty)
}

/// The in-flow heights behind the centre page rect: the find and
/// permission strips are rows above the page, so the stored page rect
/// starts below them — placement and the mouse-down release share it.
pub(crate) const FIND_STRIP_PX: f32 = 32.0;
pub(crate) const PERMISSION_STRIP_PX: f32 = 32.0;

/// How far below the centre column's top the page starts, given which
/// strips stand open.
pub(crate) fn strip_stack_px(find_open: bool, permission_open: bool) -> f32 {
    (if find_open { FIND_STRIP_PX } else { 0.0 }) + (if permission_open { PERMISSION_STRIP_PX } else { 0.0 })
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

/// One scripted fake-backend event, queued by a `--steps` verb and applied
/// to the next scripted page at build (captures only): finds, asks,
/// downloads and crashes — the states the probe entries show. The event
/// then drains through the real poll path, so a capture shows the same
/// strip a live page would — never a Baaz-side fake of backend state.
/// (Popups and open-requests need no capture: the drain is pinned
/// behaviourally against the scripted backend below.)
#[derive(Clone, Debug)]
pub(crate) enum FakePageScript {
    /// A find result for `query` ("3 of 12" with the defaults).
    Find {
        /// What was searched for.
        query: String,
        /// The 1-based match under the cursor.
        active: u32,
        /// How many matches the page holds.
        total: u32,
    },
    /// A permission ask waiting for an answer.
    Permission {
        /// The engine's id for the ask.
        id: u64,
        /// The origin asking.
        origin: String,
        /// What it asked for.
        kinds: Vec<PermissionKind>,
    },
    /// A download's latest state.
    Download(DownloadState),
    /// The renderer went away; the tab reads as crashed until the next
    /// successful navigation.
    Crashed(String),
}

impl FakePageScript {
    /// Queue this script on a fresh scripted backend, before it is boxed
    /// into its [`WebviewState`](aui_webview::WebviewState).
    pub(crate) fn apply_to_fake(&self, backend: &mut aui_webview::FakeWebBackend) {
        match self {
            FakePageScript::Find { query, active, total } => backend.script_find(query, *active, *total, true),
            FakePageScript::Permission { id, origin, kinds } => backend.script_permission(*id, origin, kinds.clone()),
            FakePageScript::Download(state) => backend.script_download(state.clone()),
            FakePageScript::Crashed(reason) => backend.script_crashed(reason),
        }
    }
}

/// Parse a `permission:<kinds>` verb rest (`camera,microphone`) into the
/// asked kinds. Unknown names answer `None` and the verb records a step
/// failure instead of scripting a request the sentence never names.
pub(crate) fn parse_permission_kinds(rest: &str) -> Option<Vec<PermissionKind>> {
    let mut kinds = Vec::new();
    for name in rest.split(',').map(str::trim) {
        let kind = match name {
            "camera" => PermissionKind::Camera,
            "microphone" => PermissionKind::Microphone,
            "geolocation" => PermissionKind::Geolocation,
            "notifications" => PermissionKind::Notifications,
            "clipboard" => PermissionKind::Clipboard,
            "midi" => PermissionKind::Midi,
            _ => return None,
        };
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    (!kinds.is_empty()).then_some(kinds)
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
        assert!(strip.toggle_case(), "flipping case re-searches the kept query");
        assert!(strip.toggle_case(), "flipping back re-searches too");
        assert!(!strip.set_query(""), "clearing the field stops nothing by itself");
        assert!(!strip.toggle_case(), "flipping case on an empty query searches nothing");
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
        let live: HashSet<TabId> = [second].into_iter().collect();
        strips.retain(&live);
        assert!(strips.visible(Some(first)).is_none(), "closed tabs keep no strip");
        assert!(strips.visible(Some(second)).is_none(), "a never-opened tab still shows nothing");
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
            "https://call.example/ wants to use your camera and microphone"
        );
        assert_eq!(
            permission_sentence(
                "https://all.example/",
                &[PermissionKind::Camera, PermissionKind::Microphone, PermissionKind::Geolocation]
            ),
            "https://all.example/ wants to use your camera, microphone and location"
        );
    }

    #[test]
    fn allow_while_visiting_auto_answers_repeats() {
        use PermissionKind::{Camera, Microphone};
        let mut allows = VisitAllows::default();
        assert_eq!(auto_answer("https://cam.example/", &[Camera], &allows), None, "strangers ask");
        allows.remember("https://cam.example/", &[Camera]);
        assert_eq!(
            auto_answer("https://cam.example/", &[Camera], &allows),
            Some(PermissionDecision::Allow),
            "trusted origins auto-allow"
        );
        assert_eq!(
            auto_answer("https://cam.example/", &[Camera, Microphone], &allows),
            None,
            "only the granted kinds skip the strip"
        );
        assert_eq!(
            auto_answer("https://other.example/", &[Camera], &allows),
            None,
            "trust is per origin"
        );
        allows.revoke_origin("https://cam.example/");
        assert_eq!(
            auto_answer("https://cam.example/", &[Camera], &allows),
            None,
            "navigating away revokes the grant"
        );
    }

    #[test]
    fn permission_origins_normalise_to_full_origins() {
        assert_eq!(
            normalize_origin("https://example.com/some/page", "https://example.com/other"),
            "https://example.com",
            "paths never join the key"
        );
        assert_eq!(
            normalize_origin("example.com", "https://example.com:8443/app"),
            "https://example.com:8443",
            "a host-only report borrows the tab's scheme and port"
        );
        assert_eq!(
            normalize_origin("example.com", "http://example.com/"),
            "http://example.com",
            "no port to borrow, none joined"
        );
    }

    #[test]
    fn downloads_badge_reads_known_totals_only() {
        fn downloading(received: u64, total: Option<u64>) -> DownloadState {
            DownloadState {
                id: 1,
                url: "https://cdn.example/big.zip".to_owned(),
                file_name: "big.zip".to_owned(),
                path: "/tmp/big.zip".to_owned(),
                received,
                total,
                state: DownloadStatus::Downloading,
            }
        }
        let idle = downloads_badge(&[]);
        assert_eq!(idle, DownloadsBadge { label: "↓".to_owned(), active: false, indeterminate: false });
        let half = downloads_badge(&[downloading(5_000, Some(10_000))]);
        assert_eq!(half.label, "50%", "known totals percent: {}", half.label);
        assert!(half.active && !half.indeterminate);
        let mixed = downloads_badge(&[downloading(5_000, Some(10_000)), downloading(9_000, None)]);
        assert!(mixed.indeterminate, "an unknown active total reads indeterminate, not a percent");
        assert_eq!(mixed.label, "…");
        let over = downloads_badge(&[downloading(12_000, Some(10_000))]);
        assert_eq!(over.label, "100%", "the badge never reads past 100: {}", over.label);
        let done = DownloadState { state: DownloadStatus::Complete, ..downloading(10_000, Some(10_000)) };
        let quiet = downloads_badge(&[done]);
        assert!(!quiet.active, "landed downloads wear no ring");
    }

    #[test]
    fn downloads_flag_clears_only_honestly() {
        assert!(downloads_flag_clears(false, true, false), "no centre tab clears");
        assert!(downloads_flag_clears(false, false, false), "no centre tab clears, even with downloads");
        assert!(downloads_flag_clears(true, true, true), "an emptied list clears once rendered");
        assert!(!downloads_flag_clears(true, true, false), "a fresh flag survives its first frame");
        assert!(!downloads_flag_clears(true, false, true), "a listing popover stays");
    }

    #[test]
    fn background_openers_cannot_take_the_centre() {
        let mut tabs = Tabs::default();
        let shown = tabs.open(None, Owner::Person, "https://a.example/", None);
        let hidden = tabs.open(None, Owner::Person, "https://b.example/", None);
        assert!(popup_foreground(true, shown, Some(shown)), "the shown tab's popups may");
        assert!(
            !popup_foreground(true, hidden, Some(shown)),
            "a background opener's foreground ask lands behind"
        );
        assert!(!popup_foreground(false, shown, Some(shown)), "background stays background");
        assert!(!popup_foreground(true, shown, None), "nothing shown, nothing takes it");
    }

    #[test]
    fn strip_stack_counts_open_rows() {
        assert_eq!(strip_stack_px(false, false), 0.0);
        assert_eq!(strip_stack_px(true, false), FIND_STRIP_PX);
        assert_eq!(strip_stack_px(true, true), FIND_STRIP_PX + PERMISSION_STRIP_PX);
    }

    #[test]
    fn find_debounce_is_per_tab() {
        let mut tabs = Tabs::default();
        let first = tabs.open(None, Owner::Person, "https://a.example/", None);
        let second = tabs.open(None, Owner::Person, "https://b.example/", None);
        let mut strips = FindStrips::default();
        let stale = strips.arm(first);
        let fresh = strips.arm(second);
        assert!(strips.armed_current(second, fresh), "the latest keystroke searches");
        assert!(!strips.armed_current(first, stale), "a switch never lets a stale tab search");
        assert!(!strips.armed_current(first, fresh), "a generation never crosses tabs");
    }

    #[test]
    fn shared_find_input_lives_while_any_strip_is_open() {
        let mut tabs = Tabs::default();
        let first = tabs.open(None, Owner::Person, "https://a.example/", None);
        let second = tabs.open(None, Owner::Person, "https://b.example/", None);
        let mut strips = FindStrips::default();
        strips.for_tab(first).open();
        strips.for_tab(second).open();
        assert!(strips.for_tab(first).close(), "closing one strip stops its search");
        assert!(strips.any_open(), "the other tab's strip keeps the input alive");
        assert!(strips.for_tab(second).close());
        assert!(!strips.any_open(), "the last close drops the input");
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
        assert_eq!(line, "4.2 of 9.7 MB · 10 s left", "sizes humanise with an estimate: {line}");
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
    fn permission_verb_parses_known_kinds_only() {
        assert_eq!(
            parse_permission_kinds("camera,microphone"),
            Some(vec![PermissionKind::Camera, PermissionKind::Microphone])
        );
        assert_eq!(parse_permission_kinds("camera,camera"), Some(vec![PermissionKind::Camera]));
        assert_eq!(parse_permission_kinds("telepathy"), None);
        assert_eq!(parse_permission_kinds(""), None);
    }

    /// Every script queues on a fresh backend: the capture seam covers
    /// finds, asks, downloads and crashes alike.
    #[test]
    fn every_script_queues_on_a_fresh_backend() {
        use aui_webview::{DownloadStatus, FakeWebBackend, WebBackend as _, WebEvent};
        let mut backend = FakeWebBackend::new();
        for script in [
            FakePageScript::Find { query: "price".to_owned(), active: 3, total: 12 },
            FakePageScript::Permission {
                id: 7,
                origin: "https://example.com".to_owned(),
                kinds: vec![PermissionKind::Camera],
            },
            FakePageScript::Download(DownloadState {
                id: 3,
                url: "https://cdn.example/big.zip".to_owned(),
                file_name: "big.zip".to_owned(),
                path: "/tmp/big.zip".to_owned(),
                received: 4_400_000,
                total: Some(10_485_760),
                state: DownloadStatus::Downloading,
            }),
            FakePageScript::Crashed("the page crashed (-1)".to_owned()),
        ] {
            script.apply_to_fake(&mut backend);
        }
        let events = backend.poll_events();
        assert!(
            events.iter().any(|event| matches!(event, WebEvent::Find(found) if found.query == "price" && found.active == 3 && found.total == 12)),
            "the find result queues: {events:?}"
        );
        assert!(
            events.iter().any(|event| matches!(event, WebEvent::Permission(request) if request.id == 7)),
            "the ask queues: {events:?}"
        );
        assert!(
            events.iter().any(|event| matches!(event, WebEvent::Download(download) if download.id == 3)),
            "the download queues: {events:?}"
        );
        assert!(
            events.iter().any(|event| matches!(event, WebEvent::Crashed(_))),
            "the crash queues: {events:?}"
        );
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
        assert_eq!(download_line(&listed[0], 4), "0.2 of 1.0 MB · 15 s left");
    }

    /// A crash shows the Baaz page and the next navigation clears it:
    /// the crash report drains first (the page reads crashed), then a
    /// navigation's announce clears it — the same event order a live
    /// crash-then-reload produces.
    #[gpui::test]
    fn crashed_shows_until_the_next_navigation(cx: &mut gpui::TestAppContext) {
        use aui_webview::WebBackend as _;
        let state = scripted_state(cx, |backend| backend.script_crashed("the page crashed (-1)"));
        cx.run_until_parked();
        assert!(
            state.update(cx, |state, _| state.crashed()).is_some(),
            "the crash shows the Baaz page"
        );
        let recovered = scripted_state(cx, |backend| {
            backend.script_crashed("the page crashed (-1)");
            backend.navigate("https://example.com/");
        });
        cx.run_until_parked();
        assert!(
            recovered.update(cx, |state, _| state.crashed()).is_none(),
            "the next navigation clears the crash"
        );
        assert_eq!(
            recovered.update(cx, |state, _| state.url().to_string()),
            "https://example.com/",
            "the navigation lands"
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
