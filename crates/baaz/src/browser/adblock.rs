//! Ad and tracker blocking's install into Chromium (docs/27 §8): the
//! background build of the engine and Chromium's request blocker
//! (`aui_cef::set_request_blocker`, only with the `cef` feature). The
//! lists, the engine, the per-site exceptions and the cosmetic filters are
//! [`baaz_browser_core::adblock`], re-exported here so
//! `crate::browser::adblock::…` names both.

// Without Chromium (`--no-default-features`) nothing installs the blocker.
#![cfg_attr(not(feature = "cef"), allow(dead_code))]

use std::sync::{Arc, OnceLock};

pub use baaz_browser_core::adblock::*;

/// Build the engine in the background and install it as Chromium's request
/// blocker; then refresh stale lists (the new lists apply next launch).
/// Once per process; a capture or test never blocks or downloads.
pub fn start(prefs: &AdblockPrefs) {
    static STARTED: OnceLock<()> = OnceLock::new();
    apply(prefs);
    if crate::runtime::browser_backend().in_memory() || cfg!(test) || STARTED.set(()).is_err() {
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

