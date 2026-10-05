//! B2a-H7: links from other apps open in Baaz; "Make Baaz your default
//! browser".
//!
//! Like `tabs_h1b.rs`, this suite cannot boot the app (baaz is a binary),
//! so it pins the wiring in source text alongside the script it ships:
//! the `Info.plist` browser claims in `scripts/bundle.sh`, the
//! `on_open_urls` registration, the scripted `open-url:` twin and its
//! probe entry, and the Settings → Browser default-browser row. The
//! routing itself (classification, queue-then-flush, tab order) is
//! pinned by unit and `#[gpui::test]` tests beside the code. Each
//! assertion fails on the pre-H7 tree.

use std::path::{Path, PathBuf};

fn manifest() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn workspace_root() -> PathBuf {
    manifest().parent().expect("crates/").parent().expect("root/").to_path_buf()
}

fn source(name: &str) -> String {
    let path = manifest().join("src").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("source reads: {}", path.display()))
}

/// The bundle lists Baaz as a browser: `CFBundleURLTypes` claiming
/// `http` and `https` (Viewer, "Web site URL") and
/// `CFBundleDocumentTypes` covering `public.html` / `public.xhtml`
/// (Viewer) — without dropping anything the plist already had.
#[test]
fn bundle_plist_claims_web_urls_and_html_documents() {
    let path = workspace_root().join("scripts").join("bundle.sh");
    let script = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("script reads: {}", path.display()));
    for needle in [
        "CFBundleURLTypes",
        "<string>http</string>",
        "<string>https</string>",
        "Web site URL",
        "CFBundleDocumentTypes",
        "public.html",
        "public.xhtml",
        "<string>Viewer</string>",
        "sh.baaz.app",
    ] {
        assert!(script.contains(needle), "bundle.sh claims browsers with {needle:?}");
    }
}

/// The app registers the platform open-URL handler, and the handler
/// only parks the URLs — the shell drains them on its next frame.
#[test]
fn the_app_parks_open_urls_for_the_shell() {
    let body = source("main.rs");
    assert!(body.contains("app.on_open_urls"), "main registers on_open_urls");
    assert!(
        body.contains("enqueue_external_urls"),
        "the callback parks into the external-URL queue"
    );
    let app = source("app.rs");
    assert!(app.contains("self.poll_external_urls(cx)"), "the frame drains the queue");
    assert!(app.contains("pending_external_urls"), "pre-shell URLs wait for sign-in");
}

/// `open-url:<url>` drives the same path as a real open-URL event, and
/// is documented like every other step verb.
#[test]
fn open_url_step_drives_the_real_open_path() {
    let steps = source("steps.rs");
    assert!(
        steps.contains("verb: \"open-url\""),
        "open-url: is a window step verb"
    );
    assert!(steps.contains("step_open_url"), "open-url: runs the ingest path");
    assert!(steps.contains("`open-url:<url>`"), "open-url: is in the documented table");
}

/// Settings grows a Browser section whose Default-browser row names the
/// live handler and offers Make default under its full label.
#[test]
fn settings_browser_row_names_the_default_and_offers_make_default() {
    let settings = source("settings.rs");
    assert!(settings.contains("(\"browser\", \"Browser\")"), "Browser is a settings section");
    assert!(settings.contains("settings_browser_page"), "Browser has its own page");
    assert!(settings.contains("Default browser"), "the row keeps its label");
    assert!(
        settings.contains("Make Baaz your default browser"),
        "the Make-default control keeps its label"
    );
    let helper = source("default_browser.rs");
    assert!(
        helper.contains("LSCopyDefaultHandlerForURLScheme"),
        "the row reads the live https handler"
    );
    assert!(
        helper.contains("LSSetDefaultHandlerForURLScheme"),
        "Make default claims http and https"
    );
    assert!(helper.contains("sh.baaz.app"), "the claim names Baaz's bundle id");
}

/// The probe drives the open-URL twin onto wikipedia, offline with no
/// new baselines.
#[test]
fn probe_covers_an_opened_link() {
    let path = workspace_root().join("scripts").join("uiprobe.py");
    let probe = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("probe reads: {}", path.display()));
    assert!(probe.contains("browse-opened-link"), "the probe names browse-opened-link");
    assert!(
        probe.contains("open-url:https://www.wikipedia.org/"),
        "the probe drives open-url: onto wikipedia"
    );
}
