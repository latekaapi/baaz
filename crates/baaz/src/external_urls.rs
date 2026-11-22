//! Links opened from other apps (B2a-H7): macOS delivers them through
//! `App::on_open_urls`, whose callback fires with no window of its own,
//! so it only enqueues into the process-wide queue below and kicks a
//! foreground drain ([`kick_external_drain`]) into
//! [`Harness::ingest_external_urls`] — the same path the scripted
//! `open-url:<url>` step drives, so the probe exercises what a Mail
//! click would trigger. URLs that arrive before the shell window exists
//! drain once it opens (`note_shell_app` kicks); URLs parked behind the
//! connect screen open when it finishes ([`Harness::drain_external_soon`]).
//! Nothing here runs per frame (task 4.5b).
//!
//! Every URL lands as a new **person tab** in the current project, the
//! window switches to Browse mode, and the app and window come to the
//! front. Before sign-in (the login screen still up) URLs wait in
//! [`BrowserView::pending_external_urls`](crate::browser::BrowserView) and open once
//! the shell is up. Several URLs open several tabs, in order, the last
//! one active. Baaz's own scheme and anything non-web are ignored with
//! a log line. Nothing here ever changes the system default browser.

use gpui::{AsyncApp, Context, Window};

use crate::app::Harness;
use crate::layout::LayoutGlobal as _;

/// What an incoming URL means for the shell. Pure, so unit tests pin
/// it without a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExternalOpen<'a> {
    /// `http`/`https`, or a `file:` URL at an HTML document: opens as a
    /// person tab.
    WebTab(&'a str),
    /// Baaz's own scheme: handed to the existing handling with a log
    /// line (none is registered yet, so it stops there).
    OwnScheme(&'a str),
    /// Anything else: ignored with a log line, never a tab.
    Ignored(&'a str),
}

/// Normalize one `on_open_urls` item (or one `open-url:` payload) before
/// classifying: trims wrapping whitespace and percent-encodes raw spaces
/// in `file:` URLs. Pure, so unit tests pin it without a window.
pub(crate) fn normalize_external_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.to_ascii_lowercase().starts_with("file:") {
        return trimmed.replace(' ', "%20");
    }
    trimmed.to_owned()
}

/// Decide what `url` (one normalized `on_open_urls` item, or one
/// `open-url:` payload) means. Scheme matches are case-insensitive, so
/// `HTTPS://…` still opens. There is deliberately no bare-path branch:
/// on macOS gpui delivers `NSURL.absoluteString` (always a URL string,
/// never a bare filesystem path), so a bare path is ignored with a log
/// line like anything else non-web.
pub(crate) fn classify_external_open(url: &str) -> ExternalOpen<'_> {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return ExternalOpen::WebTab(url);
    }
    if lower.starts_with("baaz:") {
        return ExternalOpen::OwnScheme(url);
    }
    if let Some(path) = lower.strip_prefix("file://") {
        let path = path.split(['?', '#']).next().unwrap_or(path);
        if has_html_extension(path) {
            return ExternalOpen::WebTab(url);
        }
        return ExternalOpen::Ignored(url);
    }
    ExternalOpen::Ignored(url)
}

/// Whether `path` (lowercased) names an HTML document: `.html`, `.htm`
/// or `.xhtml`. The dot must be there — a file merely named `html`
/// is not a document.
fn has_html_extension(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    file.contains('.') && matches!(file.rsplit('.').next().unwrap_or(""), "html" | "htm" | "xhtml")
}

/// Take the queued URLs when the shell is up; otherwise leave them
/// parked. Pure, so the queue-before-ready rule is pinned without a
/// window: not signed in means nothing is taken.
pub(crate) fn take_flushable_queue(signed_in: bool, queue: &mut Vec<String>) -> Vec<String> {
    if signed_in {
        std::mem::take(queue)
    } else {
        Vec::new()
    }
}

static EXTERNAL_URL_QUEUE: std::sync::OnceLock<std::sync::Mutex<Vec<String>>> =
    std::sync::OnceLock::new();

fn external_queue() -> &'static std::sync::Mutex<Vec<String>> {
    EXTERNAL_URL_QUEUE.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// Park `urls` from the `on_open_urls` callback, which owns no window.
pub(crate) fn enqueue_external_urls(urls: Vec<String>) {
    if let Ok(mut queue) = external_queue().lock() {
        queue.extend(urls);
    }
}

/// Take everything the callback parked since the last drain.
pub(crate) fn drain_external_queue() -> Vec<String> {
    external_queue().lock().map(|mut queue| std::mem::take(&mut *queue)).unwrap_or_default()
}

fn external_queue_len() -> usize {
    external_queue().lock().map(|queue| queue.len()).unwrap_or(0)
}

// The running app for the `on_open_urls` kick below: stashed once the
// first shell window opens (the callback itself owns no context).
// Thread-local because `AsyncApp` is `!Send`: both the stash (inside
// `run`, on the main thread) and the kick (the AppKit open-URLs
// delegate, on the main thread) touch it from there only.
std::thread_local! {
    static DRAIN_APP: std::cell::RefCell<Option<AsyncApp>> = const { std::cell::RefCell::new(None) };
}

/// Remember the running app so [`kick_external_drain`] can schedule the
/// drain on the foreground executor. Called once the shell window opens.
pub(crate) fn note_shell_app(app: AsyncApp) {
    DRAIN_APP.with(|slot| *slot.borrow_mut() = Some(app));
    // URLs that arrived before the window existed wait in the queue: drain
    // them now that there is a shell to open them in.
    if external_queue_len() > 0 {
        kick_external_drain();
    }
}

/// Schedule a drain on the foreground executor, straight from the
/// `on_open_urls` callback: a hidden or minimised window paints no
/// frames, so a drain that waited for one would leave the tab unopened
/// until the window comes back. A no-op until the first shell window
/// opens ([`note_shell_app`] drains what arrived before that).
pub(crate) fn kick_external_drain() {
    let app = DRAIN_APP.with(|slot| slot.borrow().clone());
    let Some(app) = app else { return };
    let executor = app.foreground_executor().clone();
    executor
        .spawn(async move {
            app.update(|cx| {
                let urls = drain_external_queue();
                for window in cx.windows() {
                    let batch = urls.clone();
                    let _ = window
                        .downcast::<gpui_kit::component::Root>()
                        .and_then(|handle| {
                            handle
                                .update(cx, |root, window, cx| {
                                    root.view()
                                        .clone()
                                        .downcast::<Harness>()
                                        .map(|harness| {
                                            harness.update(cx, |this, cx| {
                                                this.drain_external_now(batch, window, cx);
                                            });
                                        })
                                        .unwrap_or(())
                                })
                                .ok()
                        });
                }
            });
        })
        .detach();
}

impl Harness {
    /// One batch of outside URLs into the shell: web URLs become person
    /// tabs in arrival order (the last ends active); the own scheme and
    /// anything non-web are ignored with a log line. Before sign-in the
    /// web URLs wait in [`BrowserView::pending_external_urls`](crate::browser::BrowserView::pending_external_urls) and open once
    /// the shell is up ([`Self::flush_pending_external_urls`]).
    pub(crate) fn ingest_external_urls(
        &mut self,
        urls: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut web: Vec<String> = Vec::new();
        for url in urls {
            let normalized = normalize_external_url(&url);
            match classify_external_open(&normalized) {
                ExternalOpen::WebTab(tab) => web.push(tab.to_owned()),
                ExternalOpen::OwnScheme(own) => {
                    crate::baaz_log!("open-url: own-scheme url ignored (no handler): {own}")
                }
                ExternalOpen::Ignored(other) => {
                    crate::baaz_log!("open-url: ignoring non-web url: {other}")
                }
            }
        }
        if web.is_empty() {
            return;
        }
        // Parked only while the shell is not on screen; a signed-out Muse
        // no longer holds every link from other apps (docs/50 P1).
        if !self.shell_up(cx) {
            self.browser.update(cx, |browser, _| browser.pending_external_urls.extend(web));
            cx.notify();
            return;
        }
        self.open_external_tabs(&web, window, cx);
    }

    /// Open each URL as a person tab in the current project: Browse
    /// mode first, then one tab per URL in order — each open selects,
    /// so the last URL ends active — and the window to the front. The
    /// app activates too, so a click in another app lands on Baaz even
    /// when it was hidden behind it.
    fn open_external_tabs(&mut self, urls: &[String], window: &mut Window, cx: &mut Context<Self>) {
        if crate::layout::Layout::get(cx).mode != crate::layout::Mode::Browse {
            self.set_mode(crate::layout::Mode::Browse, window, cx);
        }
        for url in urls {
            if let Err(reason) = self.open_new_tab(url, window, cx) {
                crate::baaz_log!("open-url: no tab opened for {url}: {reason}");
            }
        }
        // The mode switch (a layout write) and each opened tab redraw the
        // window themselves.
        cx.activate(true);
        window.activate_window();
    }

    /// Open the URLs queued before sign-in, in arrival order. A no-op
    /// until the shell is up — while the login stands open the queue
    /// stays parked.
    pub(crate) fn flush_pending_external_urls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shell_up = self.shell_up(cx);
        let queued = self.browser.update(cx, |browser, _| take_flushable_queue(shell_up, &mut browser.pending_external_urls));
        if !queued.is_empty() {
            self.open_external_tabs(&queued, window, cx);
        }
    }

    /// Drain the process-wide open-URL queue into the shell, then open
    /// whatever waited behind the connect screen, on the next turn with the
    /// window at hand: the connect screen's Finish calls this, so URLs
    /// parked during a first run open as the shell comes up. Never per
    /// frame (task 4.5b).
    pub(crate) fn drain_external_soon(&mut self, cx: &mut Context<Self>) {
        let signed_in = self.shell_up(cx);
        if external_queue_len() == 0 && (!signed_in || self.browser.read(cx).pending_external_urls.is_empty()) {
            return;
        }
        let urls = drain_external_queue();
        self.tasks.push(cx.spawn(async move |this, cx| {
            let _ = this.update_in(cx, |this, window, cx| {
                this.drain_external_now(urls, window, cx);
            });
        }));
    }

    /// One synchronous drain with the window at hand: ingest the process
    /// queue's batch, then open whatever waited for sign-in. The connect
    /// screen's Finish reaches it through a task
    /// ([`Self::drain_external_soon`]); the foreground kick from the
    /// `on_open_urls` callback calls it directly, so a hidden window still
    /// opens the tab promptly.
    pub(crate) fn drain_external_now(
        &mut self,
        urls: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !urls.is_empty() {
            self.ingest_external_urls(urls, window, cx);
        }
        self.flush_pending_external_urls(window, cx);
    }

    #[cfg(feature = "probe")]
    /// `open-url:<url>`: the scripted form of a real open-URL event —
    /// the same classify/queue/open path `on_open_urls` drains into, so
    /// the probe exercises what a click in another app triggers. An empty
    /// URL is an `Err`; so is one the path ignores (not web, not an HTML
    /// file), since it opens no tab — it still runs the path, so the log
    /// line lands. A URL parked until the shell is up succeeds: parking is
    /// what the real event does then. Free: no turn, no wire.
    pub(crate) fn step_open_url(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let url = rest.trim();
        if url.is_empty() {
            return Err("no URL given".into());
        }
        let opens_tab = matches!(classify_external_open(&normalize_external_url(url)), ExternalOpen::WebTab(_));
        self.ingest_external_urls(vec![url.to_owned()], window, cx);
        if opens_tab { Ok(()) } else { Err(format!("`{url}` opens no tab (not a web URL)")) }
    }
}

#[cfg(feature = "probe")]
#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AppContext as _;

    #[test]
    fn http_and_https_open_tabs() {
        assert_eq!(
            classify_external_open("https://www.wikipedia.org/"),
            ExternalOpen::WebTab("https://www.wikipedia.org/")
        );
        assert_eq!(
            classify_external_open("http://example.com/page?q=1"),
            ExternalOpen::WebTab("http://example.com/page?q=1")
        );
    }

    #[test]
    fn the_scheme_match_ignores_case_and_whitespace() {
        let normalized = normalize_external_url("  HTTPS://example.com/ ");
        assert_eq!(normalized, "HTTPS://example.com/");
        assert_eq!(
            classify_external_open(&normalized),
            ExternalOpen::WebTab("HTTPS://example.com/")
        );
    }

    #[test]
    fn file_urls_with_raw_spaces_come_out_encoded() {
        let normalized = normalize_external_url("  file:///tmp/my page.html  ");
        assert_eq!(normalized, "file:///tmp/my%20page.html");
        assert!(matches!(
            classify_external_open(&normalized),
            ExternalOpen::WebTab("file:///tmp/my%20page.html")
        ));
        // Non-file URLs pass through untouched apart from the trim.
        assert_eq!(
            normalize_external_url("https://example.com/a b"),
            "https://example.com/a b"
        );
    }

    #[test]
    fn bare_paths_are_ignored_like_anything_non_web() {
        // gpui delivers `NSURL.absoluteString` — always a URL string,
        // never a bare filesystem path — so no bare-path branch exists.
        for raw in ["/tmp/page.html", "./page.html", "/tmp/photo.png"] {
            let normalized = normalize_external_url(raw);
            assert_eq!(
                classify_external_open(&normalized),
                ExternalOpen::Ignored(normalized.as_str()),
                "for {raw:?}"
            );
        }
    }

    #[test]
    fn html_files_open_tabs_and_other_files_do_not() {
        assert!(matches!(
            classify_external_open("file:///tmp/page.html"),
            ExternalOpen::WebTab(_)
        ));
        assert!(matches!(
            classify_external_open("file:///tmp/page.xhtml"),
            ExternalOpen::WebTab(_)
        ));
        assert_eq!(
            classify_external_open("file:///tmp/photo.png"),
            ExternalOpen::Ignored("file:///tmp/photo.png")
        );
        assert_eq!(
            classify_external_open("file:///tmp/no-extension"),
            ExternalOpen::Ignored("file:///tmp/no-extension")
        );
    }

    #[test]
    fn the_own_scheme_keeps_its_existing_handling() {
        assert_eq!(
            classify_external_open("baaz://settings/browser"),
            ExternalOpen::OwnScheme("baaz://settings/browser")
        );
    }

    #[test]
    fn anything_else_is_ignored_with_a_log_line() {
        for raw in ["", "javascript:alert(1)", "about:blank", "mailto:a@b.c", "ftp://host/file"] {
            assert_eq!(classify_external_open(raw), ExternalOpen::Ignored(raw.trim()), "for {raw:?}");
        }
    }

    #[test]
    fn the_queue_waits_for_sign_in_then_flushes_whole() {
        let mut queue = vec!["https://a.example/".to_owned(), "https://b.example/".to_owned()];
        assert!(take_flushable_queue(false, &mut queue).is_empty());
        assert_eq!(queue.len(), 2, "nothing leaves before the shell is up");
        assert_eq!(
            take_flushable_queue(true, &mut queue),
            vec!["https://a.example/".to_owned(), "https://b.example/".to_owned()]
        );
        assert!(queue.is_empty(), "the flush takes the whole queue");
    }

    #[test]
    fn several_urls_stay_in_arrival_order() {
        let batch = vec![
            "https://first.example/".to_owned(),
            "file:///tmp/page.html".to_owned(),
            "https://last.example/".to_owned(),
        ];
        let planned: Vec<String> = batch
            .iter()
            .filter_map(|url| match classify_external_open(url) {
                ExternalOpen::WebTab(tab) => Some(tab.to_owned()),
                _ => None,
            })
            .collect();
        assert_eq!(planned, batch, "order is arrival order; the last opens last, hence active");
    }

    /// B2a-H7 over a live shell: several outside URLs open several
    /// person tabs in order with the last one active, in Browse with
    /// the window to the front.
    #[gpui::test]
    fn outside_urls_open_person_tabs_in_order_last_active(cx: &mut gpui::TestAppContext) {
        use crate::browser::Owner;
        let state = hermetic_state("external-order");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.args.login = crate::LoginSample::SignedIn;
                h.apply_login_sample(window, cx);
            })
        });
        let urls = vec![
            "https://first.example/".to_owned(),
            "https://second.example/".to_owned(),
            "https://last.example/".to_owned(),
        ];
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| h.ingest_external_urls(urls.clone(), window, cx))
        });
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            assert_eq!(crate::layout::Layout::get(cx).mode, crate::layout::Mode::Browse, "an outside link switches to Browse");
            let space = h.browser.read(cx).browse_space();
            let tabs = h.browser.read(cx).tabs.ordered_tabs(&Owner::Person, space.as_deref());
            // Entering Browse arms its own blank tab first; the outside
            // URLs open behind it, one tab per URL in arrival order.
            let got: Vec<&str> =
                tabs.iter().map(|tab| tab.url.as_str()).filter(|url| *url != "about:blank").collect();
            assert_eq!(
                got,
                vec!["https://first.example/", "https://second.example/", "https://last.example/"],
                "one tab per URL, in arrival order"
            );
            let active = h.browser.read(cx).tabs.active_of(&Owner::Person, space.as_deref());
            let last = tabs.last().map(|tab| tab.id);
            assert_eq!(active, last, "the last URL ends active");
            assert!(h.browser.read(cx).pending_external_urls.is_empty(), "nothing queues once signed in");
        });
        restore_state(state);
    }

    /// B2a-H7b over a live shell: URLs arriving while the first-run
    /// connect screen owns the window wait, then open in order once the
    /// shell is up — through the connect screen's own Finish
    /// ([`Harness::finish_connect`]), never a direct flush call and never a
    /// frame (task 4.5b), so the test pins the path a real first run takes.
    /// (It was the capture-only login screen until docs/82 0.16 deleted it.)
    #[gpui::test]
    fn urls_before_the_shell_open_when_the_connect_screen_finishes(cx: &mut gpui::TestAppContext) {
        use crate::browser::Owner;
        let state = hermetic_state("external-queued");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.connect_screen.update(cx, |screen, _| screen.show());
                h.ingest_external_urls(
                    vec!["https://queued-one.example/".to_owned(), "https://queued-two.example/".to_owned()],
                    window,
                    cx,
                )
            })
        });
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            assert_eq!(
                h.browser.read(cx).pending_external_urls,
                vec!["https://queued-one.example/".to_owned(), "https://queued-two.example/".to_owned()],
                "pre-shell URLs wait"
            );
            let space = h.browser.read(cx).browse_space();
            assert!(
                h.browser.read(cx).tabs.ordered_tabs(&Owner::Person, space.as_deref()).is_empty(),
                "nothing opens before the shell is up"
            );
        });
        // Continue past the connect screen: no direct flush, no frame.
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.finish_connect(cx);
                h.args.login = crate::LoginSample::SignedIn;
                h.apply_login_sample(window, cx);
            })
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            assert!(h.browser.read(cx).pending_external_urls.is_empty(), "the connect screen's Finish drains the queue");
            let space = h.browser.read(cx).browse_space();
            let tabs = h.browser.read(cx).tabs.ordered_tabs(&Owner::Person, space.as_deref());
            let got: Vec<&str> =
                tabs.iter().map(|tab| tab.url.as_str()).filter(|url| *url != "about:blank").collect();
            assert_eq!(got, vec!["https://queued-one.example/", "https://queued-two.example/"]);
            let active = h.browser.read(cx).tabs.active_of(&Owner::Person, space.as_deref());
            assert_eq!(active, tabs.last().map(|tab| tab.id), "the last queued URL ends active");
        });
        restore_state(state);
    }

    // The hermetic helpers mirror `crate::app::tests` (baaz is a binary,
    // so sibling test modules cannot reach them): a fresh `BAAZ_STATE_DIR`
    // per test, restored at the end.
    fn test_args(dir: &std::path::Path) -> crate::Args {
        crate::Args {
            workspace: dir.to_path_buf(),
            workspace_explicit: true,
            provider: "echo".into(),
            provider_explicit: false,
            program: "muse".into(),
            theme: Some(aui_tokens::ThemeKind::Dark),
            screenshot: None,
            delay: std::time::Duration::from_millis(500),
            session: None,
            send: None,
            offline: true,
            replay: None,
            steps: Vec::new(),
            tier: None,
            print_tier: false,
            approval_mode: None,
            login: crate::LoginSample::Choose,
            login_steps: Vec::new(),
            bench: None,
            bench_cadence: std::time::Duration::from_millis(4),
            bench_scroll: crate::bench::BenchScroll::Sweep,
            bench_frames: 600,
            bench_out: None,
            bench_open_turn: false,
            bench_bare: false,
            bench_shell: false,
            sidebar_fixture: None,
            no_project: false,
            terminal_socket_dir: Some(crate::terminal::service::test_socket_dir()),
        }
    }

    fn hermetic_state(name: &str) -> (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("baaz-external-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("probe state dir");
        let guard = crate::store::test_env_lock();
        let old = std::env::var_os("BAAZ_STATE_DIR");
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        (guard, old, dir)
    }

    fn restore_state(
        state: (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, std::path::PathBuf),
    ) {
        let (guard, old, dir) = state;
        let _ = std::fs::remove_dir_all(&dir);
        match old {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        drop(guard);
    }
}
