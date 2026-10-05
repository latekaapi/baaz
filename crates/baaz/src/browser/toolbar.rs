//! The Browse toolbar's pure model (B2a-H4): the address states, the
//! Enter-to-URL-or-search decision, the omnibox ranking, the new-tab
//! pick-up rows, and the per-tab back/forward stacks. No gpui types here,
//! so the gate's unit tests pin every decision without a window.
//!
//! The address badge reads the library's [`PageSecurity`](aui_webview::PageSecurity)
//! through [`effective_security`]: a backend that reports nothing (`Unknown`)
//! falls back to classifying the URL itself, so the scripted page — which
//! only announces URL/title/loading on navigate — still shows Secure, Local,
//! NotSecure, File and Internal correctly. A real engine's verdict is never
//! overridden: only `Unknown` falls back.

use aui_webview::PageSecurity;

/// The search engine an address commit searches with (B2a-H4): a const
/// until Settings arrives in H6.
pub(crate) const DEFAULT_SEARCH_ENGINE: &str = "https://www.google.com/search?q=";
/// The engine's short name for the omnibox's default row.
pub(crate) const SEARCH_ENGINE_NAME: &str = "Google";

/// What Enter on the address field means: navigate, or search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NavTarget {
    /// Load this URL.
    Url(String),
    /// Search the default engine for this query.
    Search(String),
}

/// Percent-encode a search query: unreserved bytes pass through, spaces
/// become `%20`, everything else `%XX`.
fn encode_query(query: &str) -> String {
    let mut out = String::with_capacity(query.len());
    for byte in query.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(*byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Whether `text` (already known to hold no whitespace and no scheme) reads
/// as a host: `example.com`, `localhost:3000`, `127.0.0.1:5173/x`. A bare
/// word (`foo`) is a search, not a host.
fn looks_like_host(text: &str) -> bool {
    if text.is_empty() || !text.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '-' | '_' | '/' | '?' | '#' | '&' | '=' | '%' | '+' | '@')) {
        return false;
    }
    text.contains('.') || text.contains(':')
}

/// The loopback hosts that earn `http`, not `https`.
fn is_loopback_host(host: &str) -> bool {
    let host = host.split(':').next().unwrap_or(host).trim_matches(['[', ']']);
    host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") || host == "127.0.0.1" || host == "::1"
}

/// Decide what Enter on `text` does: a URL when it parses or looks like a
/// host, otherwise a search with the default engine. Empty restores the
/// field (an empty search), never navigates.
pub(crate) fn classify_address_input(text: &str) -> NavTarget {
    let typed = text.trim();
    if typed.is_empty() {
        return NavTarget::Search(String::new());
    }
    if typed.contains("://") || typed.starts_with("about:") || typed.starts_with("data:") || typed.starts_with("file:") {
        return NavTarget::Url(typed.to_owned());
    }
    if typed.chars().any(char::is_whitespace) {
        return NavTarget::Search(format!("{DEFAULT_SEARCH_ENGINE}{}", encode_query(typed)));
    }
    if looks_like_host(typed) {
        let host = typed.split('/').next().unwrap_or(typed);
        let scheme = if is_loopback_host(host) { "http" } else { "https" };
        return NavTarget::Url(format!("{scheme}://{typed}"));
    }
    NavTarget::Search(format!("{DEFAULT_SEARCH_ENGINE}{}", encode_query(typed)))
}

/// The address badge: secure gets the tune glyph (never a padlock), Local
/// the `>_ dev` tag, the rest their words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AddressBadge {
    /// Secure: the tune/sliders glyph, no label.
    Tune,
    /// `PageSecurity::Local`: `>_ dev`.
    Dev,
    /// `PageSecurity::NotSecure`: `⚠ Not secure`.
    Warn,
    /// `PageSecurity::File`: `File`.
    File,
    /// Internal/Baaz pages: `✦ Baaz`.
    Baaz,
}

/// The resting address view: badge, host and path in their voices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AddressView {
    /// Which badge leads, if any.
    pub badge: Option<AddressBadge>,
    /// The badge's words (`""` for the glyph-only Tune, `"New tab"` needs none).
    pub label: String,
    /// The bright run: the host, or the file path, or the Baaz page name.
    pub host: String,
    /// The quiet run: the path after the host (`""` when none).
    pub path: String,
}

/// The security the badge reads: the backend's verdict, except `Unknown` —
/// "the backend said nothing" — which classifies the URL itself so the
/// scripted page still badges correctly. Never overrides a real verdict.
pub(crate) fn effective_security(url: &str, reported: PageSecurity) -> PageSecurity {
    if reported == PageSecurity::Unknown {
        PageSecurity::classify(url, None)
    } else {
        reported
    }
}

/// Split `url` into host and path: the scheme stripped, the first `/`
/// dividing them. `("example.com", "/a/b")`, `("localhost:5173", "/overview")`.
fn split_host_path(url: &str) -> (String, String) {
    let after = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    match after.split_once('/') {
        Some((host, path)) => (host.to_owned(), format!("/{path}")),
        None => (after.to_owned(), String::new()),
    }
}

/// The pure address state for `url` under `security`: badge, host and path.
/// `editing` renders the full-URL input instead, so the view carries the URL
/// through untouched and no badge.
pub(crate) fn address_view(url: &str, security: PageSecurity, editing: bool) -> AddressView {
    if editing {
        return AddressView { badge: None, label: String::new(), host: url.to_owned(), path: String::new() };
    }
    match effective_security(url, security) {
        PageSecurity::Secure => {
            let (host, path) = split_host_path(url);
            AddressView { badge: Some(AddressBadge::Tune), label: String::new(), host, path }
        }
        PageSecurity::Local => {
            let (host, path) = split_host_path(url);
            AddressView { badge: Some(AddressBadge::Dev), label: ">_ dev".to_owned(), host, path }
        }
        PageSecurity::NotSecure => {
            let (host, path) = split_host_path(url);
            AddressView { badge: Some(AddressBadge::Warn), label: "⚠ Not secure".to_owned(), host, path }
        }
        PageSecurity::File => {
            let path = url.strip_prefix("file://").unwrap_or(url).to_owned();
            AddressView { badge: Some(AddressBadge::File), label: "File".to_owned(), host: String::new(), path }
        }
        PageSecurity::Internal => {
            let name = if url == "about:blank" { "New tab".to_owned() } else { url.split_once(':').map(|(_, rest)| rest.trim_start_matches('/').to_owned()).unwrap_or_else(|| url.to_owned()) };
            AddressView { badge: Some(AddressBadge::Baaz), label: "✦ Baaz".to_owned(), host: name, path: String::new() }
        }
        PageSecurity::Unknown => AddressView { badge: None, label: String::new(), host: url.to_owned(), path: String::new() },
    }
}

/// The zoom pill's text, or `None` at 100%: only a non-default zoom wears one.
pub(crate) fn zoom_pill(zoom: u16) -> Option<String> {
    if zoom == 100 { None } else { Some(format!("{zoom}%")) }
}

/// Whether `progress` means the page is still loading: strictly between idle
/// (0) and done (1). The reload button is a stop button exactly then.
pub(crate) fn is_loading(progress: f32) -> bool {
    progress > 0.0 && progress < 1.0
}

/// What an omnibox suggestion opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SuggestKind {
    /// Ask the agent (⌘↵): always first while anything is typed.
    Ask,
    /// An open tab of this project.
    Tab,
    /// A session whose name matches.
    Session,
    /// History and pages read (omitted until B2b fills it).
    History,
    /// The default "search with <engine>" row, always last.
    Search,
}

/// One omnibox row: what it opens, what it says, and its key hint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Suggestion {
    /// Which section this row belongs to.
    pub kind: SuggestKind,
    /// The row's text.
    pub title: String,
    /// The row's key hint (`"⌘↵"`, `"↵"`, `""`).
    pub hint: String,
}

/// What an omnibox row activates: the tab or session to show, the URL to
/// load, or the question to ask. Ids ride beside the display rows so Baaz
/// activates exactly the ranked row — never a second title lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OmniTarget {
    /// Ask the agent about the query text.
    Ask(String),
    /// Show this tab (a person tab: select it; a session tab: carry it).
    Tab(super::TabId),
    /// Show this session (Sessions mode, session selected).
    Session(String),
    /// Load this history URL.
    History(String),
    /// Load this search URL.
    Search(String),
}

/// One ranked row: what it says and what it does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OmniRow {
    /// The row's display.
    pub suggestion: Suggestion,
    /// The row's activation.
    pub target: OmniTarget,
}

const MAX_TAB_SUGGESTIONS: usize = 5;
const MAX_SESSION_SUGGESTIONS: usize = 5;
const MAX_HISTORY_SUGGESTIONS: usize = 4;

fn matches(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(needle)
}

/// Rank the omnibox rows for `query` over this project's open tabs
/// (`(id, title, url)`), sessions (`(id, name)`) and read history
/// (`(title, url)`): Ask first, then matching tabs and sessions, then
/// matching history — the history section omitted when empty — then the
/// engine row. The palette's lists filter on the same substring rule; this
/// mirrors it for the address field's own rows.
pub(crate) fn rank_omnibox(
    query: &str,
    tabs: &[(super::TabId, String, String)],
    sessions: &[(String, String)],
    history: &[(String, String)],
) -> Vec<OmniRow> {
    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let needle = q.to_lowercase();
    let mut rows = vec![OmniRow {
        suggestion: Suggestion { kind: SuggestKind::Ask, title: format!("Ask: {q}"), hint: "⌘↵".to_owned() },
        target: OmniTarget::Ask(q.to_owned()),
    }];
    for (id, title, url) in tabs {
        if rows.iter().filter(|row| row.suggestion.kind == SuggestKind::Tab).count() >= MAX_TAB_SUGGESTIONS {
            break;
        }
        if matches(title, &needle) || matches(url, &needle) {
            rows.push(OmniRow {
                suggestion: Suggestion { kind: SuggestKind::Tab, title: title.clone(), hint: String::new() },
                target: OmniTarget::Tab(*id),
            });
        }
    }
    for (id, name) in sessions {
        if rows.iter().filter(|row| row.suggestion.kind == SuggestKind::Session).count() >= MAX_SESSION_SUGGESTIONS {
            break;
        }
        if matches(name, &needle) {
            rows.push(OmniRow {
                suggestion: Suggestion { kind: SuggestKind::Session, title: name.clone(), hint: String::new() },
                target: OmniTarget::Session(id.clone()),
            });
        }
    }
    for (title, url) in history {
        if rows.iter().filter(|row| row.suggestion.kind == SuggestKind::History).count() >= MAX_HISTORY_SUGGESTIONS {
            break;
        }
        if matches(title, &needle) || matches(url, &needle) {
            rows.push(OmniRow {
                suggestion: Suggestion { kind: SuggestKind::History, title: title.clone(), hint: String::new() },
                target: OmniTarget::History(url.clone()),
            });
        }
    }
    rows.push(OmniRow {
        suggestion: Suggestion {
            kind: SuggestKind::Search,
            title: format!("Search with {SEARCH_ENGINE_NAME} for {q}"),
            hint: "↵".to_owned(),
        },
        target: OmniTarget::Search(format!("{DEFAULT_SEARCH_ENGINE}{}", encode_query(q))),
    });
    rows
}

/// One "Pick up where you left off" row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PickupRow {
    /// The row's title.
    pub title: String,
    /// `"harness · tab"`, `"harness · session"`, `"closed 10 min ago"`.
    pub detail: String,
}

/// The new-tab page's pick-up rows: this project's open tabs, then its
/// recent sessions, then recently closed tabs — capped at `cap` (5 on the
/// page). Closed rows carry their pre-rendered age (`"10 min ago"`).
pub(crate) fn pickup_rows(
    tabs: &[(String, String)],
    sessions: &[(String, String)],
    closed: &[(String, String)],
    cap: usize,
) -> Vec<PickupRow> {
    let mut rows = Vec::new();
    for (title, project) in tabs {
        rows.push(PickupRow { title: title.clone(), detail: format!("{project} · tab") });
    }
    for (name, project) in sessions {
        rows.push(PickupRow { title: name.clone(), detail: format!("{project} · session") });
    }
    for (title, ago) in closed {
        rows.push(PickupRow { title: title.clone(), detail: format!("closed {ago}") });
    }
    rows.truncate(cap);
    rows
}

/// How long ago `ms` milliseconds were, for closed-tab rows.
pub(crate) fn relative_ago(ms: u64) -> String {
    const MINUTE: u64 = 60_000;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    if ms < MINUTE {
        "just now".to_owned()
    } else if ms < HOUR {
        format!("{} min ago", ms / MINUTE)
    } else if ms < DAY {
        format!("{} h ago", ms / HOUR)
    } else {
        format!("{} d ago", ms / DAY)
    }
}

/// One tab's Baaz-tracked back/forward stacks (B2a-H4): the library owns the
/// real history and exposes no back/forward/​reload calls, so the header
/// toolbar tracks the URLs the sync reports and navigates within them. An
/// outside navigation (the library row's own back button, a link) lands
/// through [`Self::note_navigated`], which clears the forward stack exactly
/// like a fresh navigation does.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BackForward {
    past: Vec<String>,
    future: Vec<String>,
    current: Option<String>,
}

const HISTORY_CAP: usize = 50;

impl BackForward {
    /// Record the tab showing `url`: a new URL pushes the old current back
    /// and clears the forward stack; the same URL (or an empty one) is a
    /// no-op, so one sync per frame never grows the stacks.
    pub(crate) fn note_navigated(&mut self, url: &str) {
        if url.is_empty() || self.current.as_deref() == Some(url) {
            return;
        }
        if let Some(current) = self.current.take() {
            if current != url {
                self.past.push(current);
                if self.past.len() > HISTORY_CAP {
                    self.past.remove(0);
                }
            }
        }
        self.future.clear();
        self.current = Some(url.to_owned());
    }

    /// Whether back would go anywhere.
    pub(crate) fn can_back(&self) -> bool {
        !self.past.is_empty()
    }

    /// The back stack, oldest first: the back-history menu reverses it.
    pub(crate) fn past_urls(&self) -> Vec<String> {
        self.past.clone()
    }

    /// Whether forward would go anywhere.
    pub(crate) fn can_forward(&self) -> bool {
        !self.future.is_empty()
    }

    /// Step back: the URL to navigate to, or `None` at the start.
    pub(crate) fn go_back(&mut self) -> Option<String> {
        let prev = self.past.pop()?;
        if let Some(current) = self.current.take() {
            self.future.push(current);
        }
        self.current = Some(prev.clone());
        Some(prev)
    }

    /// Step forward: the URL to navigate to, or `None` at the end.
    pub(crate) fn go_forward(&mut self) -> Option<String> {
        let next = self.future.pop()?;
        if let Some(current) = self.current.take() {
            self.past.push(current);
        }
        self.current = Some(next.clone());
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_states_badge_host_and_path() {
        let cases: &[(&str, PageSecurity, Option<AddressBadge>, &str, &str, &str)] = &[
            ("https://example.com/docs/a", PageSecurity::Secure, Some(AddressBadge::Tune), "", "example.com", "/docs/a"),
            ("https://example.com", PageSecurity::Secure, Some(AddressBadge::Tune), "", "example.com", ""),
            ("https://example.com/docs/a", PageSecurity::Unknown, Some(AddressBadge::Tune), "", "example.com", "/docs/a"),
            ("http://localhost:5173/overview", PageSecurity::Unknown, Some(AddressBadge::Dev), ">_ dev", "localhost:5173", "/overview"),
            ("http://example.com/", PageSecurity::NotSecure, Some(AddressBadge::Warn), "⚠ Not secure", "example.com", "/"),
            ("file:///tmp/a.html", PageSecurity::Unknown, Some(AddressBadge::File), "File", "", "/tmp/a.html"),
            ("about:blank", PageSecurity::Internal, Some(AddressBadge::Baaz), "✦ Baaz", "New tab", ""),
            ("chrome://version", PageSecurity::Unknown, Some(AddressBadge::Baaz), "✦ Baaz", "version", ""),
        ];
        for (url, security, badge, label, host, path) in cases {
            let view = address_view(url, *security, false);
            assert_eq!(view.badge, *badge, "badge for {url}");
            assert_eq!(view.label, label.to_owned(), "label for {url}");
            assert_eq!(view.host, host.to_owned(), "host for {url}");
            assert_eq!(view.path, path.to_owned(), "path for {url}");
        }
    }

    #[test]
    fn editing_carries_the_url_with_no_badge() {
        let view = address_view("https://example.com/a", PageSecurity::Secure, true);
        assert_eq!(view.badge, None, "editing shows the input, never a badge");
        assert_eq!(view.host, "https://example.com/a");
    }

    #[test]
    fn reported_security_wins_over_classification() {
        // A real engine's NotSecure (a certificate error) survives even on
        // an https URL that would otherwise classify Secure.
        let view = address_view("https://example.com/", PageSecurity::NotSecure, false);
        assert_eq!(view.badge, Some(AddressBadge::Warn));
        // And a reported Secure stands on any URL.
        let view = address_view("http://example.com/", PageSecurity::Secure, false);
        assert_eq!(view.badge, Some(AddressBadge::Tune));
    }

    #[test]
    fn enter_decides_url_vs_search() {
        assert_eq!(
            classify_address_input("example.com"),
            NavTarget::Url("https://example.com".to_owned())
        );
        assert_eq!(
            classify_address_input("localhost:3000"),
            NavTarget::Url("http://localhost:3000".to_owned())
        );
        assert_eq!(
            classify_address_input("  https://x.y/z  "),
            NavTarget::Url("https://x.y/z".to_owned())
        );
        assert_eq!(
            classify_address_input("file:///tmp/a.html"),
            NavTarget::Url("file:///tmp/a.html".to_owned())
        );
        assert_eq!(
            classify_address_input("foo bar"),
            NavTarget::Search(format!("{DEFAULT_SEARCH_ENGINE}foo%20bar"))
        );
    }

    #[test]
    fn enter_edge_cases() {
        assert_eq!(classify_address_input(""), NavTarget::Search(String::new()), "empty restores");
        assert_eq!(
            classify_address_input("  "),
            NavTarget::Search(String::new()),
            "blank restores"
        );
        assert!(
            matches!(classify_address_input("foo"), NavTarget::Search(_)),
            "a bare word searches"
        );
        assert_eq!(
            classify_address_input("localhost:3000/a/b"),
            NavTarget::Url("http://localhost:3000/a/b".to_owned())
        );
        assert_eq!(
            classify_address_input("about:blank"),
            NavTarget::Url("about:blank".to_owned())
        );
    }

    #[test]
    fn omnibox_ranks_ask_tabs_sessions_history_search() {
        use super::super::TabId;
        let tabs = vec![
            (TabId(1), "Recharts API reference".to_owned(), "https://recharts.org/api".to_owned()),
            (TabId(2), "Weekend weather".to_owned(), "https://weather.example/weekend".to_owned()),
        ];
        let sessions =
            vec![("s-1".to_owned(), "Fix ledger chart".to_owned()), ("s-2".to_owned(), "Recharts spike".to_owned())];
        let history = vec![("Recharts gallery".to_owned(), "https://recharts.org/gallery".to_owned())];
        let rows = rank_omnibox("recharts", &tabs, &sessions, &history);
        let kinds: Vec<SuggestKind> = rows.iter().map(|row| row.suggestion.kind).collect();
        assert_eq!(
            kinds,
            vec![SuggestKind::Ask, SuggestKind::Tab, SuggestKind::Session, SuggestKind::History, SuggestKind::Search]
        );
        assert_eq!(rows[0].suggestion.title, "Ask: recharts");
        assert_eq!(rows[0].suggestion.hint, "⌘↵");
        assert_eq!(rows[0].target, OmniTarget::Ask("recharts".to_owned()));
        assert_eq!(rows[1].target, OmniTarget::Tab(TabId(1)));
        assert_eq!(rows[2].target, OmniTarget::Session("s-2".to_owned()));
        assert_eq!(rows[3].target, OmniTarget::History("https://recharts.org/gallery".to_owned()));
        assert!(rows.last().is_some_and(|row| row.suggestion.kind == SuggestKind::Search));
        // Non-matching tabs and sessions drop out; Ask and Search stay.
        let rows = rank_omnibox("recharts", &tabs, &sessions, &[]);
        assert!(
            !rows.iter().any(|row| row.suggestion.kind == SuggestKind::History),
            "the history section is omitted when empty"
        );
        let rows = rank_omnibox("zzz-no-match", &tabs, &sessions, &history);
        let kinds: Vec<SuggestKind> = rows.iter().map(|row| row.suggestion.kind).collect();
        assert_eq!(kinds, vec![SuggestKind::Ask, SuggestKind::Search]);
        assert!(rank_omnibox("   ", &tabs, &sessions, &history).is_empty(), "empty query suggests nothing");
    }

    #[test]
    fn pickup_rows_cap_at_five_with_details() {
        let tabs = vec![
            ("Recharts · API reference".to_owned(), "harness".to_owned()),
            ("Extra tab".to_owned(), "harness".to_owned()),
        ];
        let sessions = vec![
            ("Fix ledger chart".to_owned(), "harness".to_owned()),
            ("Second session".to_owned(), "harness".to_owned()),
        ];
        let closed = vec![
            ("Weekend weather".to_owned(), "10 min ago".to_owned()),
            ("Old tab".to_owned(), "2 h ago".to_owned()),
        ];
        let rows = pickup_rows(&tabs, &sessions, &closed, 5);
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].detail, "harness · tab");
        assert_eq!(rows[2].detail, "harness · session");
        assert_eq!(rows[4].detail, "closed 10 min ago");
        assert_eq!(rows[4].title, "Weekend weather");
    }

    #[test]
    fn relative_ages_read() {
        assert_eq!(relative_ago(0), "just now");
        assert_eq!(relative_ago(30_000), "just now");
        assert_eq!(relative_ago(600_000), "10 min ago");
        assert_eq!(relative_ago(7_200_000), "2 h ago");
        assert_eq!(relative_ago(90_000_000), "1 d ago");
    }

    #[test]
    fn back_forward_tracks_navigations() {
        let mut history = BackForward::default();
        assert!(!history.can_back() && !history.can_forward());
        history.note_navigated("https://a.example/");
        history.note_navigated("https://a.example/");
        assert!(!history.can_back(), "re-reporting the URL grows nothing");
        history.note_navigated("https://b.example/");
        assert!(history.can_back() && !history.can_forward());
        assert_eq!(history.go_back().as_deref(), Some("https://a.example/"));
        assert!(history.can_forward());
        assert_eq!(history.go_forward().as_deref(), Some("https://b.example/"));
        // An outside navigation clears the forward stack.
        assert_eq!(history.go_back().as_deref(), Some("https://a.example/"));
        history.note_navigated("https://c.example/");
        assert!(!history.can_forward(), "a fresh navigation clears forward");
        assert_eq!(history.go_back().as_deref(), Some("https://a.example/"));
        assert_eq!(history.go_back(), None, "the start goes nowhere");
    }

    #[test]
    fn zoom_pill_only_off_default() {
        assert_eq!(zoom_pill(100), None);
        assert_eq!(zoom_pill(125).as_deref(), Some("125%"));
    }

    #[test]
    fn loading_is_strictly_between_idle_and_done() {
        assert!(!is_loading(0.0));
        assert!(is_loading(0.5));
        assert!(!is_loading(1.0));
    }
}
