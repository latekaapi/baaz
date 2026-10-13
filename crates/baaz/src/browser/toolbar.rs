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


/// The engine an address commit searches with (B2a-H6): what the
/// Settings → Browser page owns, carried on
/// [`crate::layout::Layout::search_engine`]. Google is the default, like
/// the old const.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SearchEngine {
    /// Google.
    #[default]
    Google,
    /// DuckDuckGo.
    DuckDuckGo,
    /// Bing.
    Bing,
    /// Kagi.
    Kagi,
}

impl SearchEngine {
    /// Every engine the Settings page offers, in row order.
    pub(crate) const ALL: [SearchEngine; 4] =
        [SearchEngine::Google, SearchEngine::DuckDuckGo, SearchEngine::Bing, SearchEngine::Kagi];

    /// The machine name: what `layout.json` stores.
    pub(crate) fn slug(self) -> &'static str {
        match self {
            SearchEngine::Google => "google",
            SearchEngine::DuckDuckGo => "duckduckgo",
            SearchEngine::Bing => "bing",
            SearchEngine::Kagi => "kagi",
        }
    }

    /// The inverse of [`SearchEngine::slug`]: `None` for an unknown
    /// string, so a hand-edited file reads as Google, never an error.
    pub(crate) fn parse(s: &str) -> Option<SearchEngine> {
        match s.trim().to_ascii_lowercase().as_str() {
            "google" => Some(SearchEngine::Google),
            "duckduckgo" | "duck-duck-go" | "ddg" => Some(SearchEngine::DuckDuckGo),
            "bing" => Some(SearchEngine::Bing),
            "kagi" => Some(SearchEngine::Kagi),
            _ => None,
        }
    }

    /// The user-visible name: the omnibox's default row and the Settings
    /// rows.
    pub(crate) fn name(self) -> &'static str {
        match self {
            SearchEngine::Google => "Google",
            SearchEngine::DuckDuckGo => "DuckDuckGo",
            SearchEngine::Bing => "Bing",
            SearchEngine::Kagi => "Kagi",
        }
    }

    /// The search URL prefix: the query appends percent-encoded.
    pub(crate) fn url_prefix(self) -> &'static str {
        match self {
            SearchEngine::Google => "https://www.google.com/search?q=",
            SearchEngine::DuckDuckGo => "https://duckduckgo.com/?q=",
            SearchEngine::Bing => "https://www.bing.com/search?q=",
            SearchEngine::Kagi => "https://kagi.com/search?q=",
        }
    }

    /// The search URL for `query`.
    pub(crate) fn search_url(self, query: &str) -> String {
        format!("{}{}", self.url_prefix(), encode_query(query))
    }
}

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

/// Schemes the address field navigates, case-insensitively: anything else
/// with `://` or a `scheme:` prefix (`javascript:`, `data:`, `vbscript:`,
/// `blob:`, `mailto:`) is a search, never navigated.
const NAV_SCHEMES: &[&str] = &["http", "https", "file", "about", "chrome", "devtools"];
/// Non-navigated schemes with a `scheme:` prefix (no `//`): a search.
const SEARCH_SCHEMES: &[&str] = &["javascript", "data", "vbscript", "blob", "mailto", "tel"];

/// Whether `head` (the text before the first `:`) reads as a URL scheme:
/// a leading letter, then letters, digits, `+`, `-` or `.`.
fn is_scheme_shape(head: &str) -> bool {
    let mut chars = head.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() => (),
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Whether `text` (already known to hold no whitespace and no navigable
/// scheme) reads as a host: `example.com`, `münchen.de`, `localhost`,
/// `localhost:3000`, `[::1]:3000`, `127.0.0.1:8080/x`. A bare word
/// (`foo`), an address (`user@example.com`) and a decimal (`3.14`) are
/// searches, not hosts.
fn looks_like_host(text: &str) -> bool {
    if text.is_empty()
        || !text.chars().all(|c| {
            c.is_alphanumeric()
                || matches!(
                    c,
                    '.' | ':' | '-' | '_' | '/' | '?' | '#' | '&' | '=' | '%' | '+' | '[' | ']'
                )
        })
    {
        return false;
    }
    // A bare `user@host` is an address, not a URL (a real userinfo URL
    // carries a scheme and never reaches here).
    if text.contains('@') {
        return false;
    }
    // A dotted decimal is a number, not a host — unless it reads as an
    // IPv4 address.
    if text.bytes().all(|b| b.is_ascii_digit() || b == b'.') && !is_ipv4(text) {
        return false;
    }
    text.contains('.') || text.contains(':') || is_loopback_host(text)
}

/// Whether `text` reads as a dotted-quad IPv4 address.
fn is_ipv4(text: &str) -> bool {
    let parts: Vec<&str> = text.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 3
                && part.bytes().all(|b| b.is_ascii_digit())
                && part.parse::<u8>().is_ok()
        })
}

/// The loopback hosts that earn `http`, not `https`: `localhost`,
/// `*.localhost`, `127.0.0.1` and `::1`, with or without brackets, port
/// or path.
fn is_loopback_host(host: &str) -> bool {
    let host = host.split(['/', '?', '#']).next().unwrap_or(host);
    let host = host.strip_suffix('.').unwrap_or(host);
    let bare = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else if host.matches(':').count() >= 2 {
        // A bare IPv6 literal (`::1`): the colons are the address, not
        // a port.
        host
    } else {
        host.split(':').next().unwrap_or(host)
    };
    bare.eq_ignore_ascii_case("localhost")
        || bare.to_ascii_lowercase().ends_with(".localhost")
        || bare == "127.0.0.1"
        || bare == "::1"
}

/// Decide what Enter on `text` does: a URL for an allowlisted scheme or
/// something that looks like a host, otherwise a search with the
/// engine. Empty restores the field (an empty search), never navigates.
/// Dangerous schemes (`javascript:`, `data:`, …) are searches, never
/// navigated — and so is any unknown `scheme:` prefix (`ms-settings:`,
/// `ftp:`), except `host:port` (`localhost:3000`), which still navigates.
/// Bare loopbacks (`localhost`, `localhost:3000`, `[::1]:3000`,
/// `127.0.0.1:8080`) earn `http://`, everything else `https://`.
pub(crate) fn classify_address_input_with(text: &str, engine: SearchEngine) -> NavTarget {
    let typed = text.trim();
    if typed.is_empty() {
        return NavTarget::Search(String::new());
    }
    let search = |query: &str| NavTarget::Search(engine.search_url(query));
    if let Some((scheme, _)) = typed.split_once("://") {
        if NAV_SCHEMES.contains(&scheme.to_ascii_lowercase().as_str()) {
            return NavTarget::Url(typed.to_owned());
        }
        return search(typed);
    }
    if let Some((head, rest)) = typed.split_once(':') {
        if !head.contains(['.', '/', '?', '#', '@']) && is_scheme_shape(head) {
            let scheme = head.to_ascii_lowercase();
            if NAV_SCHEMES.contains(&scheme.as_str()) {
                return NavTarget::Url(typed.to_owned());
            }
            if SEARCH_SCHEMES.contains(&scheme.as_str()) {
                return search(typed);
            }
            // An unknown `scheme:` prefix (`ms-settings:`, `ftp:`) is a
            // search, never `https://scheme:...` — except `host:port`
            // (`localhost:3000`), which falls through to the host rule
            // below and still navigates.
            // `user:pass@host` is credentials, not a scheme: it falls through
            // to the host rule like `host:port` does.
            let port = rest.split(['/', '?', '#']).next().unwrap_or(rest);
            let credentials = port.contains('@');
            if !credentials && (port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit())) {
                return search(typed);
            }
        }
    }
    if typed.chars().any(char::is_whitespace) {
        return search(typed);
    }
    if looks_like_host(typed) {
        let scheme = if is_loopback_host(typed) { "http" } else { "https" };
        return NavTarget::Url(format!("{scheme}://{typed}"));
    }
    search(typed)
}




/// Split `after` (the text after `://`) into authority and path: the
/// authority ends at the first `/`, `\`, `?` or `#` — the WHATWG rule the
/// library's own host extraction uses — and the path keeps its leading
/// separator. `("example.com", "/a/b")`, `("evil.com", "\@google.com/")`.
fn split_authority_path(after: &str) -> (&str, &str) {
    match after.find(['/', '\\', '?', '#']) {
        Some(ix) => (&after[..ix], &after[ix..]),
        None => (after, ""),
    }
}

/// Split an authority into userinfo and host at the last `@`: the display
/// host is what the library classifies (userinfo stripped), so
/// `google.com@evil.com` shows `evil.com` bright and `google.com` quiet.
fn split_userinfo(authority: &str) -> (String, String) {
    match authority.rsplit_once('@') {
        Some((user, host)) => (user.to_owned(), host.to_owned()),
        None => (String::new(), authority.to_owned()),
    }
}

/// Split `url` into quiet userinfo, bright host and quiet path: the scheme
/// stripped, the authority ending at the first `/ \ ? #`, the host after
/// the last `@`. Mirrors the library's host extraction (`userinfo`
/// stripped, `\ ? #` ending the authority, host lowercased like the
/// library's own `host_of`) — only the path keeps what was typed.
pub(crate) fn split_host_path(url: &str) -> (String, String, String) {
    let after = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let (authority, path) = split_authority_path(after);
    let (userinfo, host) = split_userinfo(authority);
    (userinfo, host.to_ascii_lowercase(), path.to_owned())
}



/// Chrome's zoom ladder: the ⋯ menu's –/+ steps snap along it, never by
/// fixed points. The library (`aui_cef`) exposes no snapping of its own,
/// so the ladder lives here, beside the pill that reads it.
pub(crate) const ZOOM_LADDER: &[u16] = &[25, 33, 50, 67, 75, 80, 90, 100, 110, 125, 150, 175, 200, 250, 300, 400, 500];

/// The next rung along [`ZOOM_LADDER`]: the first rung past `zoom`
/// (`forward`), or the last rung before it (`!forward`). An off-ladder
/// zoom snaps to the nearest rung in that direction; the ends hold.
pub(crate) fn zoom_step(zoom: u16, forward: bool) -> u16 {
    if forward {
        ZOOM_LADDER.iter().copied().find(|rung| *rung > zoom).unwrap_or(*ZOOM_LADDER.last().unwrap_or(&zoom))
    } else {
        ZOOM_LADDER.iter().copied().rev().find(|rung| *rung < zoom).unwrap_or(*ZOOM_LADDER.first().unwrap_or(&zoom))
    }
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
    /// What Tab writes into the field for this row: the tab's URL, the
    /// session's title, the history URL — `None` for Ask and Search rows,
    /// which are not completable.
    pub complete: Option<String>,
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

/// How `needle` matches `haystack`: a prefix beats a word start, which
/// beats a mid-word substring. `None` is no match at all.
fn match_tier(haystack: &str, needle: &str) -> Option<u8> {
    let haystack = haystack.to_lowercase();
    if haystack.starts_with(needle) {
        return Some(0);
    }
    if haystack
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| !word.is_empty() && word.starts_with(needle))
    {
        return Some(1);
    }
    if haystack.contains(needle) {
        return Some(2);
    }
    None
}

/// The best tier over several fields (a tab's title or its URL).
fn best_tier(fields: &[&str], needle: &str) -> Option<u8> {
    fields.iter().filter_map(|field| match_tier(field, needle)).min()
}

/// Rank the omnibox rows for `query` over this project's open tabs
/// (`(id, title, url, recency_ms)`), sessions (`(id, name, recency_ms)`)
/// and read history (`(title, url, recency_ms)`): Ask first, then matching
/// tabs and sessions, then matching history — the history section omitted
/// when empty — then the engine row. Inside a section a prefix match beats
/// a word-start, which beats a substring, and recency breaks ties (later
/// `recency_ms` first; `0` is unknown and sorts last among equals).
pub(crate) fn rank_omnibox(
    query: &str,
    tabs: &[(super::TabId, String, String, u64)],
    sessions: &[(String, String, u64)],
    history: &[(String, String, u64)],
    engine: SearchEngine,
) -> Vec<OmniRow> {
    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let needle = q.to_lowercase();
    let mut rows = vec![OmniRow {
        suggestion: Suggestion {
            kind: SuggestKind::Ask,
            title: format!("Ask: {q}"),
            hint: "⌘↵".to_owned(),
            complete: None,
        },
        target: OmniTarget::Ask(q.to_owned()),
    }];
    let mut tab_rows: Vec<(u8, u64, OmniRow)> = Vec::new();
    for (id, title, url, recency) in tabs {
        if let Some(tier) = best_tier(&[title, url], &needle) {
            tab_rows.push((
                tier,
                *recency,
                OmniRow {
                    suggestion: Suggestion {
                        kind: SuggestKind::Tab,
                        title: title.clone(),
                        hint: String::new(),
                        complete: Some(url.clone()),
                    },
                    target: OmniTarget::Tab(*id),
                },
            ));
        }
    }
    tab_rows.sort_by(|left, right| left.0.cmp(&right.0).then(right.1.cmp(&left.1)));
    for (_, _, row) in tab_rows.into_iter().take(MAX_TAB_SUGGESTIONS) {
        rows.push(row);
    }
    let mut session_rows: Vec<(u8, u64, OmniRow)> = Vec::new();
    for (id, name, recency) in sessions {
        if let Some(tier) = match_tier(name, &needle) {
            session_rows.push((
                tier,
                *recency,
                OmniRow {
                    suggestion: Suggestion {
                        kind: SuggestKind::Session,
                        title: name.clone(),
                        hint: String::new(),
                        complete: Some(name.clone()),
                    },
                    target: OmniTarget::Session(id.clone()),
                },
            ));
        }
    }
    session_rows.sort_by(|left, right| left.0.cmp(&right.0).then(right.1.cmp(&left.1)));
    for (_, _, row) in session_rows.into_iter().take(MAX_SESSION_SUGGESTIONS) {
        rows.push(row);
    }
    let mut history_rows: Vec<(u8, u64, OmniRow)> = Vec::new();
    for (title, url, recency) in history {
        if let Some(tier) = best_tier(&[title, url], &needle) {
            history_rows.push((
                tier,
                *recency,
                OmniRow {
                    suggestion: Suggestion {
                        kind: SuggestKind::History,
                        title: title.clone(),
                        hint: String::new(),
                        complete: Some(url.clone()),
                    },
                    target: OmniTarget::History(url.clone()),
                },
            ));
        }
    }
    history_rows.sort_by(|left, right| left.0.cmp(&right.0).then(right.1.cmp(&left.1)));
    for (_, _, row) in history_rows.into_iter().take(MAX_HISTORY_SUGGESTIONS) {
        rows.push(row);
    }
    rows.push(OmniRow {
        suggestion: Suggestion {
            kind: SuggestKind::Search,
            title: format!("Search with {} for {q}", engine.name()),
            hint: "↵".to_owned(),
            complete: None,
        },
        target: OmniTarget::Search(engine.search_url(q)),
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
/// real history and exposes no back/forward/​reload calls (its engine
/// stepping is private to the pane), so the header toolbar tracks the URLs
/// the sync reports and navigates within them. An outside navigation (a
/// link, the pane's own controls before Browse hid them) lands through
/// [`Self::note_navigated`], which clears the forward stack exactly like a
/// fresh navigation does.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BackForward {
    past: Vec<String>,
    future: Vec<String>,
    current: Option<String>,
    /// The URL the page still shows after [`Self::go_back`] stepped the
    /// stacks ahead of it: the per-frame sync reports the old URL until
    /// the navigation lands, and that stale echo must not undo the step.
    /// Consumed once — a genuine later navigation to the same URL heals
    /// on the next frame, since the sync re-reports it.
    stale: Option<String>,
}

const HISTORY_CAP: usize = 50;

impl BackForward {
    /// Record the tab showing `url`: a new URL pushes the old current back
    /// and clears the forward stack; the same URL (or an empty one) is a
    /// no-op, so one sync per frame never grows the stacks. The one stale
    /// echo after a [`Self::go_back`]/[`Self::go_forward`] step is ignored,
    /// so the step is never undone by the page reporting late.
    pub(crate) fn note_navigated(&mut self, url: &str) {
        if url.is_empty() {
            return;
        }
        if self.current.as_deref() == Some(url) {
            // The stacks already read this URL: settled, so any armed
            // stale echo is over.
            self.stale = None;
            return;
        }
        if self.stale.as_deref() == Some(url) {
            // The pre-navigation echo of a step: swallow once.
            self.stale = None;
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
        self.stale = None;
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

    /// Step back: the URL to navigate to, or `None` at the start. Arms
    /// the stale echo (the URL the page still shows) so the per-frame
    /// [`Self::note_navigated`] cannot undo the step.
    pub(crate) fn go_back(&mut self) -> Option<String> {
        let prev = self.past.pop()?;
        if let Some(current) = self.current.take() {
            if self.stale.is_none() {
                self.stale = Some(current.clone());
            }
            self.future.push(current);
        }
        self.current = Some(prev.clone());
        Some(prev)
    }

    /// Step forward: the URL to navigate to, or `None` at the end. Arms
    /// the stale echo like [`Self::go_back`] does.
    pub(crate) fn go_forward(&mut self) -> Option<String> {
        let next = self.future.pop()?;
        if let Some(current) = self.current.take() {
            if self.stale.is_none() {
                self.stale = Some(current.clone());
            }
            self.past.push(current);
        }
        self.current = Some(next.clone());
        Some(next)
    }

    /// Step back `steps` entries at once: what picking a back-history menu
    /// row does, so the stacks stay right (the entries above the target
    /// move forward, the target becomes current). `0` steps back once.
    pub(crate) fn go_back_steps(&mut self, steps: usize) -> Option<String> {
        let mut target = None;
        for _ in 0..steps.max(1) {
            target = self.go_back();
            if target.is_none() {
                break;
            }
        }
        target
    }
}

/// The Ask draft text for `query` over `existing`: an empty composer takes
/// the question as is, otherwise the question appends on a new line —
/// never replacing, never sent.
pub(crate) fn ask_draft_text(existing: &str, query: &str) -> String {
    if existing.trim().is_empty() {
        query.to_owned()
    } else {
        format!("{}\n{query}", existing.trim_end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;




    #[test]
    fn enter_decides_url_vs_search() {
        assert_eq!(
            classify_address_input_with("example.com", SearchEngine::Google),
            NavTarget::Url("https://example.com".to_owned())
        );
        assert_eq!(
            classify_address_input_with("localhost:3000", SearchEngine::Google),
            NavTarget::Url("http://localhost:3000".to_owned())
        );
        assert_eq!(
            classify_address_input_with("  https://x.y/z  ", SearchEngine::Google),
            NavTarget::Url("https://x.y/z".to_owned())
        );
        assert_eq!(
            classify_address_input_with("file:///tmp/a.html", SearchEngine::Google),
            NavTarget::Url("file:///tmp/a.html".to_owned())
        );
        assert_eq!(
            classify_address_input_with("foo bar", SearchEngine::Google),
            NavTarget::Search(SearchEngine::Google.search_url("foo bar"))
        );
    }

    #[test]
    fn enter_edge_cases() {
        assert_eq!(classify_address_input_with("", SearchEngine::Google), NavTarget::Search(String::new()), "empty restores");
        assert_eq!(
            classify_address_input_with("  ", SearchEngine::Google),
            NavTarget::Search(String::new()),
            "blank restores"
        );
        assert!(
            matches!(classify_address_input_with("foo", SearchEngine::Google), NavTarget::Search(_)),
            "a bare word searches"
        );
        assert_eq!(
            classify_address_input_with("localhost:3000/a/b", SearchEngine::Google),
            NavTarget::Url("http://localhost:3000/a/b".to_owned())
        );
        assert_eq!(
            classify_address_input_with("about:blank", SearchEngine::Google),
            NavTarget::Url("about:blank".to_owned())
        );
    }

    #[test]
    fn omnibox_ranks_ask_tabs_sessions_history_search() {
        use super::super::TabId;
        let tabs = vec![
            (TabId(1), "Recharts API reference".to_owned(), "https://recharts.org/api".to_owned(), 0),
            (TabId(2), "Weekend weather".to_owned(), "https://weather.example/weekend".to_owned(), 0),
        ];
        let sessions = vec![
            ("s-1".to_owned(), "Fix ledger chart".to_owned(), 0),
            ("s-2".to_owned(), "Recharts spike".to_owned(), 0),
        ];
        let history = vec![("Recharts gallery".to_owned(), "https://recharts.org/gallery".to_owned(), 0)];
        let rows = rank_omnibox("recharts", &tabs, &sessions, &history, SearchEngine::Google);
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
        let rows = rank_omnibox("recharts", &tabs, &sessions, &[], SearchEngine::Google);
        assert!(
            !rows.iter().any(|row| row.suggestion.kind == SuggestKind::History),
            "the history section is omitted when empty"
        );
        let rows = rank_omnibox("zzz-no-match", &tabs, &sessions, &history, SearchEngine::Google);
        let kinds: Vec<SuggestKind> = rows.iter().map(|row| row.suggestion.kind).collect();
        assert_eq!(kinds, vec![SuggestKind::Ask, SuggestKind::Search]);
        assert!(rank_omnibox("   ", &tabs, &sessions, &history, SearchEngine::Google).is_empty(), "empty query suggests nothing");
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
    fn zoom_steps_follow_chromes_ladder() {
        assert_eq!(zoom_step(100, true), 110, "plus leaves 100 for 110, never 120");
        assert_eq!(zoom_step(100, false), 90, "minus leaves 100 for 90, never 80");
        assert_eq!(zoom_step(110, false), 100);
        assert_eq!(zoom_step(90, true), 100);
        assert_eq!(zoom_step(96, true), 100, "off-ladder snaps forward to the next rung");
        assert_eq!(zoom_step(96, false), 90, "off-ladder snaps back to the rung below");
        assert_eq!(zoom_step(500, true), 500, "the top rung holds");
        assert_eq!(zoom_step(25, false), 25, "the bottom rung holds");
        assert_eq!(zoom_step(400, true), 500);
    }

    #[test]
    fn classify_table_allowlists_schemes_and_loopbacks() {
        type Case<'a> = (&'a str, NavTarget);
        let search = |query: &str| NavTarget::Search(SearchEngine::Google.search_url(query));
        let cases: &[Case] = &[
            ("http://example.com/a", NavTarget::Url("http://example.com/a".to_owned())),
            ("HTTP://example.com/a", NavTarget::Url("HTTP://example.com/a".to_owned())),
            ("https://example.com", NavTarget::Url("https://example.com".to_owned())),
            ("file:///tmp/a.html", NavTarget::Url("file:///tmp/a.html".to_owned())),
            ("about:blank", NavTarget::Url("about:blank".to_owned())),
            ("About:Blank", NavTarget::Url("About:Blank".to_owned())),
            ("chrome://version", NavTarget::Url("chrome://version".to_owned())),
            ("devtools://devtools/bundled/inspector.html", NavTarget::Url("devtools://devtools/bundled/inspector.html".to_owned())),
            ("example.com", NavTarget::Url("https://example.com".to_owned())),
            ("münchen.de", NavTarget::Url("https://münchen.de".to_owned())),
            ("localhost", NavTarget::Url("http://localhost".to_owned())),
            ("localhost:3000", NavTarget::Url("http://localhost:3000".to_owned())),
            ("[::1]:3000", NavTarget::Url("http://[::1]:3000".to_owned())),
            ("127.0.0.1:8080", NavTarget::Url("http://127.0.0.1:8080".to_owned())),
            ("1.2.3.4", NavTarget::Url("https://1.2.3.4".to_owned())),
            ("javascript:alert(1)", search("javascript:alert(1)")),
            ("JAVASCRIPT:alert(1)", search("JAVASCRIPT:alert(1)")),
            ("data:text/html,<h1>hi</h1>", search("data:text/html,<h1>hi</h1>")),
            ("vbscript:msgbox(1)", search("vbscript:msgbox(1)")),
            ("blob:https://example.com/1", search("blob:https://example.com/1")),
            ("javascript://example.com/x", search("javascript://example.com/x")),
            ("user@example.com", search("user@example.com")),
            ("3.14", search("3.14")),
            ("foo", search("foo")),
            ("foo bar", search("foo bar")),
        ];
        for (input, expected) in cases {
            assert_eq!(
                &classify_address_input_with(input, SearchEngine::Google),
                expected,
                "classify {input:?}"
            );
        }
    }


    #[test]
    fn omnibox_tiers_prefix_wordstart_substring_then_recency() {
        use super::super::TabId;
        let tabs = vec![
            (TabId(1), "A tapir story".to_owned(), "https://hive.example/1".to_owned(), 30),
            (TabId(2), "The best apiary".to_owned(), "https://hive.example/2".to_owned(), 20),
            (TabId(3), "Apiary".to_owned(), "https://hive.example/3".to_owned(), 10),
        ];
        let rows = rank_omnibox("api", &tabs, &[], &[], SearchEngine::Google);
        let ids: Vec<TabId> = rows
            .iter()
            .filter_map(|row| match row.target {
                OmniTarget::Tab(id) => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec![TabId(3), TabId(2), TabId(1)], "prefix, then word-start, then substring");
        // Same tier: the more recently shown tab wins.
        let tabs = vec![
            (TabId(1), "Apiary one".to_owned(), "https://hive.example/1".to_owned(), 10),
            (TabId(2), "Apiary two".to_owned(), "https://hive.example/2".to_owned(), 50),
        ];
        let rows = rank_omnibox("api", &tabs, &[], &[], SearchEngine::Google);
        let ids: Vec<TabId> = rows
            .iter()
            .filter_map(|row| match row.target {
                OmniTarget::Tab(id) => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec![TabId(2), TabId(1)], "recency breaks prefix ties");
    }

    #[test]
    fn omnibox_completion_text_by_row_kind() {
        use super::super::TabId;
        let tabs = vec![(TabId(7), "Ledger".to_owned(), "https://ledger.example/".to_owned(), 0)];
        let sessions = vec![("s-9".to_owned(), "Ledger fix".to_owned(), 0)];
        let rows = rank_omnibox("ledger", &tabs, &sessions, &[], SearchEngine::Google);
        let tab = rows.iter().find(|row| row.suggestion.kind == SuggestKind::Tab).expect("a tab row");
        assert_eq!(tab.suggestion.complete.as_deref(), Some("https://ledger.example/"), "Tab completes the URL");
        let session =
            rows.iter().find(|row| row.suggestion.kind == SuggestKind::Session).expect("a session row");
        assert_eq!(session.suggestion.complete.as_deref(), Some("Ledger fix"), "sessions complete the title");
        for kind in [SuggestKind::Ask, SuggestKind::Search] {
            let row = rows.iter().find(|row| row.suggestion.kind == kind).expect("the row");
            assert_eq!(row.suggestion.complete, None, "{kind:?} rows are not completable");
        }
    }

    #[test]
    fn back_step_survives_the_stale_echo() {
        let mut history = BackForward::default();
        history.note_navigated("https://a.example/");
        history.note_navigated("https://b.example/");
        history.note_navigated("https://c.example/");
        assert_eq!(history.go_back_steps(2).as_deref(), Some("https://a.example/"));
        assert!(history.can_forward());
        // The page still shows the old URL until the navigation lands.
        history.note_navigated("https://c.example/");
        assert!(!history.can_back(), "the stale echo steps nothing");
        assert!(history.can_forward(), "the forward stack survives the echo");
        assert_eq!(history.go_forward().as_deref(), Some("https://b.example/"));
        history.note_navigated("https://b.example/");
        assert!(history.can_back() && history.can_forward());
    }

    #[test]
    fn ask_draft_appends_on_a_new_line() {
        assert_eq!(ask_draft_text("", "what is this page?"), "what is this page?");
        assert_eq!(ask_draft_text("   ", "what is this page?"), "what is this page?");
        assert_eq!(
            ask_draft_text("summarise this", "what is this page?"),
            "summarise this\nwhat is this page?"
        );
    }

    #[test]
    fn unknown_scheme_prefixes_are_a_search() {
        // `ms-settings:` and `ftp:` are not navigated — and never gain an
        // `https://` prefix — while a host with a numeric port still does.
        let search = |query: &str| NavTarget::Search(SearchEngine::Google.search_url(query));
        for input in ["ms-settings:privacy", "MS-SETTINGS:privacy", "ftp:files", "gopher:hole"] {
            assert_eq!(
                classify_address_input_with(input, SearchEngine::Google),
                search(input),
                "classify {input:?}"
            );
        }
        assert_eq!(
            classify_address_input_with("localhost:3000", SearchEngine::Google),
            NavTarget::Url("http://localhost:3000".to_owned()),
            "a numeric port still reads as a host"
        );
    }


    /// B2a-H6: every engine builds its own search URL — names, slugs
    /// and parse round-trip, and queries encode the same everywhere.
    #[test]
    fn search_engines_build_their_own_urls() {
        assert_eq!(SearchEngine::default(), SearchEngine::Google);
        assert_eq!(
            SearchEngine::ALL.map(|engine| engine.slug()),
            ["google", "duckduckgo", "bing", "kagi"]
        );
        assert_eq!(
            SearchEngine::ALL.map(|engine| engine.name()),
            ["Google", "DuckDuckGo", "Bing", "Kagi"]
        );
        assert_eq!(SearchEngine::parse("Google"), Some(SearchEngine::Google));
        assert_eq!(SearchEngine::parse(" duckduckgo "), Some(SearchEngine::DuckDuckGo));
        assert_eq!(SearchEngine::parse("BING"), Some(SearchEngine::Bing));
        assert_eq!(SearchEngine::parse("kagi"), Some(SearchEngine::Kagi));
        assert_eq!(SearchEngine::parse("yahoo"), None);
        assert_eq!(
            SearchEngine::Google.search_url("foo bar"),
            "https://www.google.com/search?q=foo%20bar"
        );
        assert_eq!(
            SearchEngine::DuckDuckGo.search_url("foo bar"),
            "https://duckduckgo.com/?q=foo%20bar"
        );
        assert_eq!(
            SearchEngine::Bing.search_url("a/b?c"),
            "https://www.bing.com/search?q=a%2Fb%3Fc"
        );
        assert_eq!(SearchEngine::Kagi.search_url("foo bar"), "https://kagi.com/search?q=foo%20bar");
    }

    /// B2a-H6: Enter searches the Settings engine — URLs never touch it.
    #[test]
    fn classify_with_engine_searches_there() {
        assert_eq!(
            classify_address_input_with("foo bar", SearchEngine::Kagi),
            NavTarget::Search("https://kagi.com/search?q=foo%20bar".to_owned())
        );
        assert_eq!(
            classify_address_input_with("example.com", SearchEngine::Bing),
            NavTarget::Url("https://example.com".to_owned()),
            "a host navigates whatever the engine"
        );
        assert_eq!(
            classify_address_input_with("", SearchEngine::DuckDuckGo),
            NavTarget::Search(String::new()),
            "empty still restores"
        );
    }

    /// B2a-H6: the omnibox's engine row names the Settings engine and
    /// loads its URL.
    #[test]
    fn omnibox_engine_row_names_the_settings_engine() {
        let rows = rank_omnibox("ledger", &[], &[], &[], SearchEngine::DuckDuckGo);
        let last = rows.last().expect("a query always ranks the engine row");
        assert_eq!(last.suggestion.title, "Search with DuckDuckGo for ledger");
        assert_eq!(last.target, OmniTarget::Search("https://duckduckgo.com/?q=ledger".to_owned()));
    }
}
