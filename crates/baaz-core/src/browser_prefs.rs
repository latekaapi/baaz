//! The browser's settings that live in `layout.json` (Settings › Browser):
//! the search engine, the assistant's provider and model, ad blocking and
//! the agents' limits. Plain data the core's [`crate::layout::Layout`]
//! carries; the browser in the app reads them.

use serde::{Deserialize, Serialize};

pub mod agents;

/// The folder browser chats start in, under Baaz's state dir. Not
/// `side-sessions`: a session there is taken for a throwaway side session.
pub const BROWSER_CHATS_DIR: &str = "browser-chats";

/// Settings › Browser › Assistant: the provider and model a new browser
/// chat starts with. `None` is "the default": the first connected
/// provider's cheapest model, the auto-titles rule (docs/27 §6).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantPrefs {
    /// The provider's wire id (`claude-code`, `codex`, `meta`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The model id as the provider takes it (`haiku`, `muse-spark-1.3`);
    /// empty is the provider's own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// The engine an address commit searches with (B2a-H6): what the
/// Settings → Browser page owns, carried on
/// [`crate::layout::Layout::search_engine`]. Google is the default, like
/// the old const.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchEngine {
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
    pub const ALL: [SearchEngine; 4] =
        [SearchEngine::Google, SearchEngine::DuckDuckGo, SearchEngine::Bing, SearchEngine::Kagi];

    #[cfg(feature = "probe")]
    /// The engine for its machine name (what `layout.json` stores, serde's
    /// lowercase): `None` for an unknown string, so a hand-edited file reads as Google, never an error.
    pub fn parse(s: &str) -> Option<SearchEngine> {
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
    pub fn name(self) -> &'static str {
        match self {
            SearchEngine::Google => "Google",
            SearchEngine::DuckDuckGo => "DuckDuckGo",
            SearchEngine::Bing => "Bing",
            SearchEngine::Kagi => "Kagi",
        }
    }

    /// The search URL prefix: the query appends percent-encoded.
    pub fn url_prefix(self) -> &'static str {
        match self {
            SearchEngine::Google => "https://www.google.com/search?q=",
            SearchEngine::DuckDuckGo => "https://duckduckgo.com/?q=",
            SearchEngine::Bing => "https://www.bing.com/search?q=",
            SearchEngine::Kagi => "https://kagi.com/search?q=",
        }
    }

    /// The search URL for `query`.
    pub fn search_url(self, query: &str) -> String {
        format!("{}{}", self.url_prefix(), encode_query(query))
    }
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

/// Settings › Browser › Ad blocking.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdblockPrefs {
    /// Blocking is on (the default).
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Sites where ads are allowed (hosts, without `www.`), sorted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed: Vec<String>,
}

fn yes() -> bool {
    true
}

impl Default for AdblockPrefs {
    fn default() -> Self {
        Self { enabled: true, allowed: Vec::new() }
    }
}

impl AdblockPrefs {
    /// Whether ads are allowed on `host` (the site or a subdomain of it).
    /// The blocker itself reads `allowed` natively (CEF); this is the rule
    /// in Rust, for the tests that pin it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn allows(&self, host: &str) -> bool {
        let host = host.strip_prefix("www.").unwrap_or(host);
        self.allowed.iter().any(|site| host == site || host.ends_with(&format!(".{site}")))
    }

    /// Flip the exception for `host`'s site. Returns whether ads are now allowed.
    pub fn toggle(&mut self, host: &str) -> bool {
        let site = host.strip_prefix("www.").unwrap_or(host).to_owned();
        if let Some(at) = self.allowed.iter().position(|s| *s == site) {
            self.allowed.remove(at);
            false
        } else {
            self.allowed.push(site);
            self.allowed.sort();
            true
        }
    }
}
