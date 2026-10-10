//! Links opened from other apps (B2a-H7): macOS delivers them through
//! `App::on_open_urls`, whose callback fires with no window of its own,
//! so it only enqueues into the process-wide queue below and kicks a
//! foreground drain ([`kick_external_drain`]). The shell also drains
//! that queue on every frame ([`Harness::poll_external_urls`]) into
//! [`Harness::ingest_external_urls`] — the same path the scripted
//! `open-url:<url>` step drives, so the probe exercises what a Mail
//! click would trigger.
//!
//! Every URL lands as a new **person tab** in the current project, the
//! window switches to Browse mode, and the app and window come to the
//! front. Before sign-in (the login screen still up) URLs wait in
//! [`Harness::pending_external_urls`](crate::app::Harness) and open once
//! the shell is up. Several URLs open several tabs, in order, the last
//! one active. Baaz's own scheme and anything non-web are ignored with
//! a log line. Nothing here ever changes the system default browser.

use gpui::{AsyncApp, Context, Window};

use crate::app::Harness;
use crate::login::Auth;

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
}

/// Schedule a drain on the foreground executor, straight from the
/// `on_open_urls` callback: a hidden or minimised window paints no
/// frames, so the frame hook alone would leave the tab unopened until
/// the window comes back. A no-op until the first shell window opens
/// (the frame hook covers URLs that arrive before that).
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
    /// web URLs wait in [`Self::pending_external_urls`] and open once
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
        if !matches!(self.auth, Auth::SignedIn(_)) {
            self.pending_external_urls.extend(web);
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
        if self.mode != crate::layout::Mode::Browse {
            self.set_mode(crate::layout::Mode::Browse, window, cx);
        }
        for url in urls {
            self.step_tab_new(url, window, cx);
        }
        cx.activate(true);
        window.activate_window();
        cx.notify();
    }

    /// Open the URLs queued before sign-in, in arrival order. A no-op
    /// until the shell is up — while the login stands open the queue
    /// stays parked.
    pub(crate) fn flush_pending_external_urls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let signed_in = matches!(self.auth, Auth::SignedIn(_));
        let queued = take_flushable_queue(signed_in, &mut self.pending_external_urls);
        if !queued.is_empty() {
            self.open_external_tabs(&queued, window, cx);
        }
    }

    /// Drain the process-wide open-URL queue into the shell, then open
    /// whatever waited for sign-in: the frame's rejoin for the
    /// `on_open_urls` callback, which fires with no window of its own.
    /// Off the frame through a task, never inside render. The pending
    /// queue flushes on any signed-in frame even when the process queue
    /// is empty — otherwise URLs parked at the login screen would wait
    /// for another click that never comes.
    pub(crate) fn poll_external_urls(&mut self, cx: &mut Context<Self>) {
        let signed_in = matches!(self.auth, Auth::SignedIn(_));
        if external_queue_len() == 0 && (!signed_in || self.pending_external_urls.is_empty()) {
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
    /// queue's batch, then open whatever waited for sign-in. The frame
    /// hook reaches it through a task ([`Self::poll_external_urls`]);
    /// the foreground kick from the `on_open_urls` callback calls it
    /// directly, so a hidden window still opens the tab promptly.
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

    /// `open-url:<url>`: the scripted form of a real open-URL event —
    /// the same classify/queue/open path `on_open_urls` drains into, so
    /// the probe exercises what a click in another app triggers. Empty
    /// records a step failure. Free: no turn, no wire.
    pub(crate) fn step_open_url(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        let url = rest.trim();
        if url.is_empty() {
            crate::steps::record_step_failure("open-url:");
            return;
        }
        self.ingest_external_urls(vec![url.to_owned()], window, cx);
    }
}

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
            cx.new(|cx| Harness::new(test_args(&state.2), crate::shot::CaptureToken::default(), window, cx))
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
            assert_eq!(h.mode, crate::layout::Mode::Browse, "an outside link switches to Browse");
            let space = h.browse_space();
            let tabs = h.browser.tabs.ordered_tabs(&Owner::Person, space.as_deref());
            // Entering Browse arms its own blank tab first; the outside
            // URLs open behind it, one tab per URL in arrival order.
            let got: Vec<&str> =
                tabs.iter().map(|tab| tab.url.as_str()).filter(|url| *url != "about:blank").collect();
            assert_eq!(
                got,
                vec!["https://first.example/", "https://second.example/", "https://last.example/"],
                "one tab per URL, in arrival order"
            );
            let active = h.browser.tabs.active_of(&Owner::Person, space.as_deref());
            let last = tabs.last().map(|tab| tab.id);
            assert_eq!(active, last, "the last URL ends active");
            assert!(h.pending_external_urls.is_empty(), "nothing queues once signed in");
        });
        restore_state(state);
    }

    /// B2a-H7b over a live shell: URLs arriving at the login screen
    /// wait, then the frame hook opens them in order once the shell is
    /// up — through [`Harness::poll_external_urls`], never a direct
    /// flush call, so the test pins the path a real sign-in takes.
    #[gpui::test]
    fn urls_before_sign_in_flush_through_poll(cx: &mut gpui::TestAppContext) {
        use crate::browser::Owner;
        let state = hermetic_state("external-queued");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::shot::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
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
                h.pending_external_urls,
                vec!["https://queued-one.example/".to_owned(), "https://queued-two.example/".to_owned()],
                "pre-shell URLs wait"
            );
            let space = h.browse_space();
            assert!(
                h.browser.tabs.ordered_tabs(&Owner::Person, space.as_deref()).is_empty(),
                "nothing opens before the shell is up"
            );
        });
        // The sign-in transition, then frames: no direct flush anywhere.
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.args.login = crate::LoginSample::SignedIn;
                h.apply_login_sample(window, cx);
            })
        });
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| h.poll_external_urls(cx));
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            assert!(h.pending_external_urls.is_empty(), "the frame hook drains the queue");
            let space = h.browse_space();
            let tabs = h.browser.tabs.ordered_tabs(&Owner::Person, space.as_deref());
            let got: Vec<&str> =
                tabs.iter().map(|tab| tab.url.as_str()).filter(|url| *url != "about:blank").collect();
            assert_eq!(got, vec!["https://queued-one.example/", "https://queued-two.example/"]);
            let active = h.browser.tabs.active_of(&Owner::Person, space.as_deref());
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
            theme: aui_tokens::ThemeKind::Dark,
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
