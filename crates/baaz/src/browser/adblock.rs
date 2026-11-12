//! R4 ad and tracker blocking (docs/27 §8, owner 11 Nov: bundle EasyList +
//! EasyPrivacy and refresh them weekly).
//!
//! The lists ship inside the app (gzip, `resources/adblock/`); a fresher copy
//! downloaded into `Browser/adblock/` wins once present. The engine
//! (Brave's adblock-rust) is built on a background thread — parsing ~130k
//! rules takes about a second — and then installed as Chromium's request
//! blocker (`aui_cef::set_request_blocker`), which is asked for every
//! sub-resource load on Chromium's IO thread. Navigations are never blocked.
//! Per-site exceptions ("Allow ads on this site") and the on/off switch live
//! in [`AdblockPrefs`] and are read through a lock-free-enough `RwLock`.

// Without Chromium (`--no-default-features`) nothing installs the blocker.
#![cfg_attr(not(feature = "cef"), allow(dead_code))]

use std::collections::HashSet;
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};

use serde::{Deserialize, Serialize};

/// The two lists: (name, bundled gzip, source URL).
const LISTS: [(&str, &[u8], &str); 2] = [
    ("easylist", include_bytes!("../../resources/adblock/easylist.txt.gz"), "https://easylist.to/easylist/easylist.txt"),
    ("easyprivacy", include_bytes!("../../resources/adblock/easyprivacy.txt.gz"), "https://easylist.to/easylist/easyprivacy.txt"),
];

/// A downloaded list older than this is fetched again.
const REFRESH_SECS: u64 = 7 * 24 * 3600;

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

/// What the IO-thread blocker reads: the switch and the allowed sites.
struct Live {
    enabled: bool,
    allowed: HashSet<String>,
}

fn live() -> &'static RwLock<Live> {
    static LIVE: OnceLock<RwLock<Live>> = OnceLock::new();
    LIVE.get_or_init(|| RwLock::new(Live { enabled: true, allowed: HashSet::new() }))
}

/// Push changed prefs to the blocker (takes effect on the next request).
pub fn apply(prefs: &AdblockPrefs) {
    if let Ok(mut live) = live().write() {
        live.enabled = prefs.enabled;
        live.allowed = prefs.allowed.iter().cloned().collect();
    }
}

fn host_allowed(live: &Live, initiator: &str) -> bool {
    let host = crate::browser::history::host_of(initiator);
    live.allowed.iter().any(|site| host == *site || host.ends_with(&format!(".{site}")))
}

/// `Browser/adblock/`: the downloaded lists.
fn dir() -> PathBuf {
    crate::store::support_dir().join("Browser").join("adblock")
}

/// One list's text: the downloaded copy when present, else the bundled one.
fn list_text(name: &str, bundled: &[u8]) -> String {
    if !(crate::clock::deterministic() || cfg!(test)) {
        if let Ok(text) = std::fs::read_to_string(dir().join(format!("{name}.txt"))) {
            if text.starts_with("[Adblock") {
                return text;
            }
        }
    }
    let mut text = String::new();
    let _ = flate2::read::GzDecoder::new(bundled).read_to_string(&mut text);
    text
}

/// Build an engine from list texts.
pub fn engine_from(lists: &[String]) -> adblock::Engine {
    let mut set = adblock::lists::FilterSet::new(false);
    for text in lists {
        set.add_filter_list(text.clone(), adblock::lists::ParseOptions::default());
    }
    adblock::Engine::new_with_filter_set(set)
}

/// Whether `engine` blocks a load of `url` (of ABP `kind`) made by `initiator`.
pub fn should_block(engine: &adblock::Engine, url: &str, initiator: &str, kind: &str) -> bool {
    let source = if initiator.is_empty() { url } else { initiator };
    match adblock::request::Request::new(url, source, kind, "GET") {
        Ok(request) => engine.check_network_request(&request).should_block(),
        Err(_) => false,
    }
}

// ---------------------------------------------------------------- cosmetics
//
// Blocking a request leaves the box the page laid out for it: an empty grey
// slot under an "ADVERTISEMENT" label (owner, 12 Nov, Indian Express). The
// lists' element-hiding rules (`##selector`) hide those boxes. The site's own
// rules come from the URL alone; the generic ones are matched against the
// classes and ids the page actually has (`COSMETIC_SCAN_JS`), and the result
// goes in as one stylesheet (`inject_js`).

/// The engine the request blocker uses, for the cosmetic lookups.
fn engine_slot() -> &'static RwLock<Option<Arc<adblock::Engine>>> {
    static ENGINE: OnceLock<RwLock<Option<Arc<adblock::Engine>>>> = OnceLock::new();
    ENGINE.get_or_init(|| RwLock::new(None))
}

/// Reads the page's distinct classes and ids (capped), as a JSON string.
pub const COSMETIC_SCAN_JS: &str = "var c = new Set(), i = new Set(); \
var all = document.querySelectorAll('[class],[id]'); \
for (var k = 0; k < all.length && k < 20000; k++) { var e = all[k]; \
  if (e.id) i.add(e.id); \
  if (e.classList) for (var j = 0; j < e.classList.length; j++) c.add(e.classList[j]); } \
return JSON.stringify({ c: Array.from(c).slice(0, 5000), i: Array.from(i).slice(0, 5000) });";

/// The stylesheet that hides what `engine`'s rules match on `url` with these
/// classes and ids; empty when nothing matches.
pub fn cosmetic_css(engine: &adblock::Engine, url: &str, classes: &[String], ids: &[String]) -> String {
    let resources = engine.url_cosmetic_resources(url);
    let mut selectors: Vec<String> = resources.hide_selectors.into_iter().collect();
    if !resources.generichide {
        selectors.extend(engine.hidden_class_id_selectors(classes, ids, &resources.exceptions));
    }
    selectors.sort();
    selectors.dedup();
    // One rule per selector: a selector this Chromium cannot parse drops only
    // its own rule, never the whole list.
    selectors.iter().map(|s| format!("{s}{{display:none!important}}\n")).collect()
}

/// [`cosmetic_css`] for a page now, honouring the switch and the site's
/// allowance; `None` when ads are allowed there or the engine is not ready.
pub fn page_css(url: &str, classes: &[String], ids: &[String]) -> Option<String> {
    {
        let live = live().read().ok()?;
        if !live.enabled || host_allowed(&live, url) {
            return None;
        }
    }
    let engine = engine_slot().read().ok()?.clone()?;
    let css = cosmetic_css(&engine, url, classes, ids);
    (!css.is_empty()).then_some(css)
}

/// The script that installs (or replaces) Baaz's hiding stylesheet.
pub fn inject_js(css: &str) -> String {
    let css = serde_json::to_string(css).unwrap_or_else(|_| "\"\"".into());
    format!(
        "var s = document.getElementById('baaz-adblock') || document.createElement('style'); \
s.id = 'baaz-adblock'; s.textContent = {css}; \
(document.head || document.documentElement).appendChild(s); return true;"
    )
}

/// Build the engine in the background and install it as Chromium's request
/// blocker; then refresh stale lists (the new lists apply next launch).
/// Once per process; a capture or test never blocks or downloads.
pub fn start(prefs: &AdblockPrefs) {
    static STARTED: OnceLock<()> = OnceLock::new();
    apply(prefs);
    if crate::clock::deterministic() || cfg!(test) || STARTED.set(()).is_err() {
        return;
    }
    std::thread::Builder::new()
        .name("adblock".into())
        .spawn(|| {
            let started = std::time::Instant::now();
            let texts: Vec<String> = LISTS.iter().map(|(name, bundled, _)| list_text(name, bundled)).collect();
            let engine = Arc::new(engine_from(&texts));
            if let Ok(mut slot) = engine_slot().write() {
                *slot = Some(engine.clone());
            }
            crate::baaz_log!("adblock: {} rules ready in {} ms", texts.iter().map(|t| t.lines().count()).sum::<usize>(), started.elapsed().as_millis());
            #[cfg(feature = "cef")]
            {
                let engine = engine.clone();
                aui_cef::set_request_blocker(Some(Arc::new(move |url: &str, initiator: &str, kind: &str| {
                    let Ok(live) = live().read() else { return false };
                    if !live.enabled || host_allowed(&live, initiator) {
                        return false;
                    }
                    drop(live);
                    let block = should_block(&engine, url, initiator, kind);
                    if block {
                        // The first block, then every 10th: proof in the log
                        // without a line per request.
                        static BLOCKED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                        let n = BLOCKED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        if n == 1 || n % 10 == 0 {
                            crate::baaz_log!("adblock: {n} requests blocked (latest {kind} {})", crate::browser::history::host_of(url));
                        }
                    }
                    block
                })));
            }
            let _ = engine;
            refresh_lists();
        })
        .ok();
}

/// Download any list older than a week (curl, no shell; failures keep the
/// copy there is).
fn refresh_lists() {
    let dir = dir();
    let _ = std::fs::create_dir_all(&dir);
    for (name, _, url) in LISTS {
        let path = dir.join(format!("{name}.txt"));
        let fresh = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age.as_secs() < REFRESH_SECS);
        if fresh {
            continue;
        }
        let part = dir.join(format!("{name}.txt.part"));
        let ok = std::process::Command::new("/usr/bin/curl")
            .args(["-fsSL", "--max-time", "60", "-o"])
            .arg(&part)
            .arg(url)
            .status()
            .is_ok_and(|s| s.success());
        let valid = ok && std::fs::read_to_string(&part).is_ok_and(|t| t.starts_with("[Adblock") && t.lines().count() > 1000);
        if valid {
            let _ = std::fs::rename(&part, &path);
            crate::baaz_log!("adblock: refreshed {name}");
        } else {
            let _ = std::fs::remove_file(&part);
            crate::baaz_log!("adblock: could not refresh {name}; keeping the copy there is");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> adblock::Engine {
        engine_from(&["||ads.example.com^\n||tracker.example^$third-party\n@@||ads.example.com/ok.js".to_owned()])
    }

    #[test]
    fn blocks_listed_hosts_and_honours_exceptions() {
        let e = engine();
        assert!(should_block(&e, "https://ads.example.com/banner.js", "https://news.example.org/", "script"));
        assert!(!should_block(&e, "https://ads.example.com/ok.js", "https://news.example.org/", "script"), "@@ exception");
        assert!(!should_block(&e, "https://cdn.example.org/app.js", "https://news.example.org/", "script"));
        assert!(should_block(&e, "https://tracker.example/p.gif", "https://news.example.org/", "image"), "third party");
        assert!(!should_block(&e, "https://tracker.example/p.gif", "https://tracker.example/", "image"), "first party");
    }

    #[test]
    fn the_bundled_lists_parse_and_block_a_known_ad_host() {
        let texts: Vec<String> = LISTS.iter().map(|(name, bundled, _)| list_text(name, bundled)).collect();
        assert!(texts.iter().all(|t| t.starts_with("[Adblock") && t.lines().count() > 10_000));
        let e = engine_from(&texts);
        assert!(should_block(&e, "https://securepubads.g.doubleclick.net/tag/js/gpt.js", "https://news.example.org/", "script"));
        assert!(!should_block(&e, "https://news.example.org/style.css", "https://news.example.org/", "stylesheet"));
    }

    /// Element hiding: a site's own rules by URL, generic rules only for the
    /// classes and ids the page has, and a site's `#@#` exception honoured.
    #[test]
    fn cosmetic_rules_hide_ad_boxes_by_site_and_by_class() {
        let e = engine_from(&[
            "news.example.org##.top-ad-slot\n##.ad-banner\n###sidebar-ad\nnews.example.org#@#.ad-banner".to_owned(),
        ]);
        let none: Vec<String> = Vec::new();
        let css = cosmetic_css(&e, "https://news.example.org/a", &none, &none);
        assert!(css.contains(".top-ad-slot{display:none!important}"), "{css}");
        let css = cosmetic_css(&e, "https://other.example/", &["ad-banner".into(), "story".into()], &["sidebar-ad".into()]);
        assert!(css.contains(".ad-banner{") && css.contains("#sidebar-ad{"), "{css}");
        assert!(!css.contains("story") && !css.contains("top-ad-slot"), "only what matches here: {css}");
        let css = cosmetic_css(&e, "https://news.example.org/", &["ad-banner".into()], &none);
        assert!(!css.contains(".ad-banner{"), "the site's exception: {css}");
        // The bundled lists hide a common generic slot.
        let texts: Vec<String> = LISTS.iter().map(|(name, bundled, _)| list_text(name, bundled)).collect();
        let full = engine_from(&texts);
        let css = cosmetic_css(&full, "https://news.example.org/", &["adsbygoogle-wrapper".into()], &none);
        assert!(css.contains(".adsbygoogle-wrapper{"), "EasyList hides a generic slot");
        // …and the owner's page by its own rules (12 Nov: empty boxes).
        let css = cosmetic_css(&full, "https://indianexpress.com/", &none, &none);
        assert!(css.contains(".ie-adsbox{") && css.contains(".adsizes{"), "Indian Express's own rules");
        assert!(inject_js("a{}\n").contains("\"a{}\\n\""));
    }

    #[test]
    fn per_site_exceptions_toggle_and_cover_subdomains() {
        let mut prefs = AdblockPrefs::default();
        assert!(prefs.toggle("www.example.com"));
        assert!(prefs.allows("example.com") && prefs.allows("news.example.com") && prefs.allows("www.example.com"));
        assert!(!prefs.allows("example.org"));
        assert!(!prefs.toggle("example.com"));
        assert!(prefs.allowed.is_empty());
        let json = serde_json::to_string(&AdblockPrefs::default()).unwrap();
        assert_eq!(serde_json::from_str::<AdblockPrefs>("{}").unwrap(), AdblockPrefs::default(), "{json}");
    }
}
