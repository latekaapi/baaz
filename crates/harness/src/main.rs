//! # harness
//!
//! A macOS chat interface to Meta's Muse Code agent, built on the `aui`
//! component library.
//!
//! ```bash
//! cargo run -p harness -- --workspace ~/code/thing
//! cargo run -p harness -- --replay fixtures/msp/transcript-approve.jsonl
//! ```
//!
//! ## `echo` is not a free provider
//!
//! On a machine that is signed in, `--provider echo` picks a *route*, not a
//! free ride. The session log proves it: `~/.local/share/muse/sessions/…/
//! session.jsonl` records `provider_id: echo` on the `command_intake` record
//! and then a metadata record naming `provider_id: meta` with a real
//! `model_id` (`muse-spark-1.3-contributor`); the session index's
//! `provider_id` follows that model record, not the intake. Turns on `echo`
//! bill reasoning tokens, carry provider response ids, and come back with
//! varied real replies. **Every turn on every provider is a real subscription
//! turn.**
//!
//! The two things that genuinely cost nothing are `--replay <capture.jsonl>`,
//! which folds a checked-in capture with no server at all, and `--no-connect`,
//! which draws the chrome without one. Starting a session and `session/
//! userShell` (the `!` path) also make no model call, which is why the whole
//! approval flow can be exercised without spending a turn — but the turn that
//! *follows* an approval does spend one.
//!
//! The window boots exactly the way `aui/examples/minimal.rs` does — the asset
//! source, then `aui::init`, then the text scale, then the window — because
//! that order is the library's contract and nothing here has earned an
//! exception to it.

#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

mod app;
mod attachments;
mod auth;
mod bench;
mod billing;
mod clock;
mod conn;
mod dialogs;
mod files;
mod full_output;
mod history;
mod images;
mod index;
mod layout;
mod log;
mod login;
mod overlays;
mod plan;
mod resize;
mod search;
mod session;
mod sessions;
mod shot;
mod sidebar;
mod sidebar_view;
mod skills;
mod steps;
mod store;
mod tier;
mod transcript;
mod wire;

use std::path::PathBuf;
use std::time::Duration;

use aui_tokens::{scale, AuiTheme, ThemeKind};
use gpui::{px, size, AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowOptions};
use gpui_kit::component::{Root, TitleBar};

/// The window the spec asks for.
const WINDOW_W: f32 = 1440.0;
/// The window height, and the transcript list's overdraw: a viewport's worth
/// of rows stay measured past the visible edge, so a wheel flick never
/// outruns measured rows into zero-height territory (see `SessionView`'s
/// list construction).
pub(crate) const WINDOW_H: f32 = 900.0;
const WINDOW_MIN_W: f32 = 900.0;
const WINDOW_MIN_H: f32 = 600.0;
/// How long a `--screenshot` render waits for fonts, layout and the first
/// frames to settle.
const SHOT_DELAY: Duration = Duration::from_millis(600);

/// Which login-screen state `--no-connect` boots into, for captures. Only
/// sample data is ever shown: the URL, the code and the key-shaped field
/// text are the example values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoginSample {
    /// The method choice (the default).
    #[default]
    Choose,
    /// The device flow mid-poll, with the example URL and code.
    Device,
    /// The API-key form, with a sample string in the masked field.
    ApiKey,
    /// The API-key form with an inline rejection.
    ApiKeyError,
    /// `account/loginStart {apiKey}` in flight.
    Validating,
    /// A failed flow, with both ways out.
    Error,
}

/// Everything the command line and the environment decide.
#[derive(Clone, Debug)]
pub struct Args {
    /// The workspace every session in this window runs in.
    pub workspace: PathBuf,
    /// `meta`, or `echo` under `HARNESS_PROVIDER=echo`.
    pub provider: String,
    /// The `muse` binary to drive.
    pub program: String,
    /// Which theme to open in.
    pub theme: ThemeKind,
    /// `--screenshot <png>`: render, save and quit.
    pub screenshot: Option<PathBuf>,
    /// How long to wait before that capture.
    pub delay: Duration,
    /// `--session <id>`: resume this session at boot; `latest` (scripting
    /// only — a screenshot cannot know a session id in advance) picks the
    /// most recently updated one in the workspace.
    pub session: Option<String>,
    /// `--send <text>`: send one turn once a session is open. The scripting
    /// hook a screenshot needs, modelled on the gallery's `AUI_GALLERY_STEPS`.
    /// Scripting only, and it reaches `turn/start` — **it costs a real turn**
    /// on the login's tier, same as the `send:`/`steer:` steps below.
    pub send: Option<String>,
    /// `--no-connect`: render the chrome without spawning `muse serve`, which
    /// is what a screenshot of the login screen wants.
    pub offline: bool,
    /// `--replay <capture.jsonl>`: fold a wire capture and render it, with no
    /// child at all (implies `--no-connect`).
    ///
    /// Most of the phase-4 screenshots are taken this way. It costs nothing —
    /// no provider, no turn, no session — and it is reproducible to the byte,
    /// because the input is a file that is checked in. Commands issued against
    /// a replayed session are refused with a banner rather than silently
    /// dropped.
    pub replay: Option<PathBuf>,
    /// `--steps <a;b;c>`: what to do to the open session before the capture.
    ///
    /// One step per item, `;`-separated because a step's payload may contain a
    /// comma. Every Phase 3 screenshot is one of these, so every screenshot is
    /// reproducible from a command line rather than from a pointer. **The
    /// whole surface is scripting only**, and the `steps` module is the one
    /// place that documents it: the verb table, which verbs the window owns
    /// and which the session does, and which of them cost a turn (only
    /// `send:` and `steer:` do, like `--send` above; every other step is
    /// free).
    pub steps: Vec<String>,
    /// `--tier subscription|payg|unknown`: skip the billing probe and pretend
    /// it said this.
    ///
    /// The probe drives the `muse` TUI in a pseudo-terminal, which takes
    /// seconds and depends on which login the machine is holding — neither of
    /// which a reproducible capture of the pay-as-you-go banner can live with.
    /// It fakes nothing else: the footer row, the banner and `/status` all read
    /// the same [`tier::Tier`] the real probe returns.
    pub tier: Option<tier::Tier>,
    /// `--print-tier`: run the billing probe, print what it found and exit,
    /// without opening a window. Costs nothing.
    pub print_tier: bool,
    /// `--approval-mode <mode>`: the mode every session this window starts is
    /// **started** in.
    ///
    /// Not the same thing as the `setmode:` step, which changes a running
    /// session's mode: `session/start` is the only surface that declares a
    /// non-interactive run's policy, and it is what a scripted capture of an
    /// approval needs — a shell command under `promptUnmatched` raises a real,
    /// server-minted approval and makes no model call.
    pub approval_mode: Option<muse_client::schema::ApprovalMode>,
    /// `--login <state>`: which login-screen state `--no-connect` boots
    /// into for a capture — `choose` (the default), `device`, `apikey`,
    /// `apikey-error`, `validating` or `error`. Honoured only without a
    /// connection; a live boot always starts at the method choice and lets
    /// `account/read` decide.
    pub login: LoginSample,
    /// `--login-steps <a;b;c>`: what to do on the login screen before the
    /// capture. Honoured only when the app is really connected (never with
    /// `--no-connect` / `--replay`), run once the login screen is up, one
    /// step per item, the same `;`-separated parsing as `--steps`. Scripting
    /// only, like `--steps` above; none of these steps reaches `turn/start`,
    /// so none of them costs anything. The verbs are in the `steps` module
    /// with the rest of the scripting surface.
    ///
    /// The key a `key-from-env:<VAR>` step carries travels from the
    /// environment into the field and then into the wire call: it never
    /// appears in argv, a log or a screenshot argument. After sign-in the
    /// ordinary `--steps` run as today, so
    /// `--login-steps 'apikey;key-from-env:MUSE_TEST_KEY;submit;wait:8000'
    /// --screenshot …` captures the signed-in shell on the API-key lane.
    pub login_steps: Vec<String>,
    /// `--bench <capture.jsonl>`: stream the capture's `<--` lines through
    /// the fold on a timer while driving the transcript list, and report
    /// element, frame and fold-apply timing plus peak RSS. Free: no child,
    /// no server (implies `--no-connect`), and it implies
    /// `HARNESS_FRAME_STATS`. See `docs/02-app.md` §6.
    pub bench: Option<PathBuf>,
    /// `--bench-cadence-ms <ms>`: the delay between streamed events.
    pub bench_cadence: Duration,
    /// `--bench-scroll top|mid|tail|sweep|wheel`: how the list is driven.
    /// `wheel` dispatches real `ScrollWheelEvent`s at the transcript centre
    /// after the stream lands (the scroll-jank instrument); the rest drive
    /// `scroll_to` while the stream lands. See `docs/02-app.md` §6.
    pub bench_scroll: bench::BenchScroll,
    /// `--bench-frames <n>`: the minimum frames to observe before stopping.
    pub bench_frames: usize,
    /// `--bench-out <file.json>`: write the numbers as one JSON object.
    pub bench_out: Option<PathBuf>,
    /// `--bench-open-turn`: stop the stream before the capture's last
    /// `turn/completed`, so the idle window is measured with a turn still
    /// running (finding `performance-13`).
    pub bench_open_turn: bool,
}

fn parse_args() -> Args {
    let mut args = std::env::args().skip(1);
    let mut out = Args {
        workspace: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        // The spec caps real turns. `echo` does not dodge that cap — it is
        // routed to the real model on a signed-in machine (see the module
        // header) — but it is the cheapest route and the one scripted runs
        // take, so a run that forgot to name a provider lands here rather than
        // on `meta` with its longer, costlier answers.
        provider: std::env::var("HARNESS_PROVIDER").unwrap_or_else(|_| "meta".into()),
        program: std::env::var("HARNESS_MUSE").unwrap_or_else(|_| "muse".into()),
        theme: ThemeKind::Dark,
        screenshot: None,
        delay: SHOT_DELAY,
        session: None,
        send: None,
        offline: false,
        replay: None,
        steps: Vec::new(),
        tier: None,
        print_tier: false,
        approval_mode: None,
        login: LoginSample::Choose,
        login_steps: Vec::new(),
        bench: None,
        bench_cadence: Duration::from_millis(4),
        bench_scroll: bench::BenchScroll::Sweep,
        bench_frames: 600,
        bench_out: None,
        bench_open_turn: false,
    };
    // Resolve it once, here: `session/list` filters on exact path equality and
    // the metadata record carries the path the server resolved, so `/tmp/x`
    // and `/private/tmp/x` are two different workspaces to the wire.
    let canonical = |p: PathBuf| p.canonicalize().unwrap_or(p);
    let mut provider_explicit = std::env::var_os("HARNESS_PROVIDER").is_some();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--workspace" => {
                let value = args.next().unwrap_or_else(|| usage("--workspace needs a path"));
                out.workspace = PathBuf::from(shellexpand(&value));
            }
            "--provider" => {
                out.provider = args.next().unwrap_or_else(|| usage("--provider needs an id"));
                provider_explicit = true;
            }
            "--theme" => {
                let value = args.next().unwrap_or_default();
                out.theme = ThemeKind::parse(&value).unwrap_or_else(|| usage(&format!("unknown theme `{value}`")));
            }
            "--screenshot" => {
                let value = args.next().unwrap_or_else(|| usage("--screenshot needs <out.png>"));
                out.screenshot = Some(PathBuf::from(value));
            }
            "--screenshot-delay" => {
                let value = args.next().unwrap_or_default();
                let ms: u64 = value.parse().unwrap_or_else(|_| usage("--screenshot-delay needs milliseconds"));
                out.delay = Duration::from_millis(ms);
            }
            "--session" => out.session = Some(args.next().unwrap_or_else(|| usage("--session needs an id or `latest`"))),
            "--send" => out.send = Some(args.next().unwrap_or_else(|| usage("--send needs the prompt text"))),
            "--steps" => {
                let value = args.next().unwrap_or_else(|| usage("--steps needs `;`-separated steps"));
                out.steps = value.split(';').filter(|s| !s.is_empty()).map(str::to_owned).collect();
            }
            "--tier" => {
                let value = args.next().unwrap_or_default();
                out.tier = Some(match value.as_str() {
                    "subscription" => tier::Tier::Subscription {
                        plan: "Muse Code High Usage".into(),
                        current_pct: Some(2),
                        weekly_pct: Some(2),
                        resets: vec!["Resets at 3:00 PM".into(), "Resets Monday".into()],
                    },
                    "payg" => tier::Tier::PayAsYouGo,
                    "unknown" => tier::Tier::Unavailable("scripted".into()),
                    other => usage(&format!("--tier takes subscription|payg|unknown, not `{other}`")),
                });
            }
            "--print-tier" => out.print_tier = true,
            "--approval-mode" => {
                use muse_client::schema::ApprovalMode;
                let value = args.next().unwrap_or_default();
                out.approval_mode = Some(
                    [
                        ApprovalMode::AllowAll,
                        ApprovalMode::OnRequest,
                        ApprovalMode::PromptUnmatched,
                        ApprovalMode::DenyUnmatched,
                    ]
                    .into_iter()
                    .find(|mode| mode.as_wire().eq_ignore_ascii_case(&value))
                    .unwrap_or_else(|| usage(&format!("unknown approval mode `{value}`"))),
                );
            }
            "--login" => {
                let value = args.next().unwrap_or_default();
                out.login = match value.as_str() {
                    "choose" => LoginSample::Choose,
                    "device" => LoginSample::Device,
                    "apikey" => LoginSample::ApiKey,
                    "apikey-error" => LoginSample::ApiKeyError,
                    "validating" => LoginSample::Validating,
                    "error" => LoginSample::Error,
                    other => usage(&format!("--login takes choose|device|apikey|apikey-error|validating|error, not `{other}`")),
                };
            }
            "--login-steps" => {
                let value = args.next().unwrap_or_else(|| usage("--login-steps needs `;`-separated steps"));
                out.login_steps = value.split(';').filter(|s| !s.is_empty()).map(str::to_owned).collect();
            }
            "--no-connect" => out.offline = true,
            "--replay" => {
                let value = args.next().unwrap_or_else(|| usage("--replay needs <capture.jsonl>"));
                out.replay = Some(PathBuf::from(shellexpand(&value)));
                out.offline = true;
            }
            "--bench" => {
                let value = args.next().unwrap_or_else(|| usage("--bench needs <capture.jsonl>"));
                out.bench = Some(PathBuf::from(shellexpand(&value)));
                out.offline = true;
            }
            "--bench-cadence-ms" => {
                let value = args.next().unwrap_or_default();
                let ms: u64 = value.parse().unwrap_or_else(|_| usage("--bench-cadence-ms needs milliseconds"));
                out.bench_cadence = Duration::from_millis(ms);
            }
            "--bench-scroll" => {
                let value = args.next().unwrap_or_default();
                out.bench_scroll = bench::BenchScroll::parse(&value)
                    .unwrap_or_else(|| usage("--bench-scroll takes top|mid|tail|sweep|wheel"));
            }
            "--bench-frames" => {
                let value = args.next().unwrap_or_default();
                out.bench_frames = value.parse().unwrap_or_else(|_| usage("--bench-frames needs a frame count"));
            }
            "--bench-open-turn" => out.bench_open_turn = true,
            "--bench-out" => {
                let value = args.next().unwrap_or_else(|| usage("--bench-out needs <file.json>"));
                out.bench_out = Some(PathBuf::from(value));
            }
            "-h" | "--help" => usage(""),
            other => usage(&format!("unknown argument `{other}`")),
        }
    }
    out.workspace = canonical(out.workspace);

    // A bench run is its own window: it streams rather than replaying, so a
    // screenshot, a step list, a send and a second capture make no sense
    // beside it. Fail loudly rather than silently ignoring them.
    if out.bench.is_some() {
        if out.screenshot.is_some() {
            usage("--bench takes no --screenshot");
        }
        if !out.steps.is_empty() || !out.login_steps.is_empty() || out.send.is_some() {
            usage("--bench takes no --steps, --login-steps or --send");
        }
        if out.replay.is_some() {
            usage("--bench takes no --replay");
        }
    }

    // Scripted runs — screenshots, `--steps`, `--send` — are how a phase burns
    // real turns by accident: Phase 3 spent 25 against a cap of five because the
    // screenshot commands omitted `HARNESS_PROVIDER=echo`. A scripted run is
    // therefore `echo` unless the provider was named explicitly.
    let scripted = out.screenshot.is_some()
        || !out.steps.is_empty()
        || !out.login_steps.is_empty()
        || out.send.is_some()
        || out.replay.is_some();
    if scripted && !provider_explicit && out.provider != "echo" {
        eprintln!(
            "harness: scripted run, routing through `echo` (still a real turn if it sends one; \
             use --replay for a free run, or --provider meta to pick the model)"
        );
        out.provider = "echo".into();
    }
    out
}

fn usage(err: &str) -> ! {
    if !err.is_empty() {
        eprintln!("error: {err}\n");
    }
    eprintln!(
        "usage: harness [--workspace <path>] [--provider <id>] [--theme light|dark]\n\
         \x20              [--session <id>|latest] [--send <text>] [--steps <a;b;c>]\n\
         \x20              [--screenshot <out.png>] [--screenshot-delay <ms>] [--no-connect]\n\
         \x20              [--replay <capture.jsonl>] [--tier subscription|payg|unknown]\n\
         \x20              [--print-tier] [--approval-mode <mode>]\n\
         \x20              [--login <state>] [--login-steps <a;b;c>]\n\
         \x20              [--bench <capture.jsonl>] [--bench-cadence-ms <ms>] [--bench-scroll top|mid|tail|sweep|wheel]\n\
         \x20              [--bench-frames <n>] [--bench-open-turn] [--bench-out <file.json>]\n\n\
         environment: HARNESS_PROVIDER=echo routes through echo (NOT free: on a signed-in\n\
         \x20              machine it reaches the real model); HARNESS_MUSE names the binary.\n\
         \x20              --replay and --no-connect are the only runs that cost nothing.\n\n\
         --send, --steps and --login-steps are scripting-only surfaces (see the `Args`\n\
         \x20              doc comments in main.rs for the full step tables). --send and the\n\
         \x20              `send:`/`steer:` steps cost a real turn; every other step is free."
    );
    std::process::exit(if err.is_empty() { 0 } else { 2 });
}

/// `--bench`: boot the bench window — one replayed session, streamed on a
/// timer — and drive it. The boot order is the library's contract, exactly as
/// in [`main`]: assets, `aui::init`, text scale, keys, menus, then the
/// window. Free: no child is ever spawned and no server is ever dialled.
fn run_bench(args: Args) {
    // Frame stats before the first frame: the flag is read once, so forcing
    // it after boot would miss the run.
    session::enable_frame_stats_for_bench();
    let theme = args.theme;
    let opts = bench::BenchOptions {
        capture: args.bench.clone().unwrap_or_else(|| usage("--bench needs <capture.jsonl>")),
        cadence: args.bench_cadence,
        scroll: args.bench_scroll,
        frames: args.bench_frames,
        out: args.bench_out.clone(),
        open_turn: args.bench_open_turn,
    };
    let command = std::env::args().collect::<Vec<_>>().join(" ");
    let (workspace, provider) = (
        args.workspace.to_string_lossy().into_owned(),
        args.provider.clone(),
    );
    gpui_kit::application().with_assets(aui::assets::AuiAssets).run(move |cx| {
        aui::init(theme, cx);
        // Same reduced-motion hold as the shell boot: a deterministic bench
        // measures stable numbers.
        if crate::clock::deterministic() {
            cx.set_reduce_motion(true);
        }
        AuiTheme::set_text_scale(scale::TEXT_SCALE, None, cx);
        app::bind_keys(cx);
        app::set_menus(cx);

        let bounds = Bounds::new(gpui::point(px(0.0), px(0.0)), size(px(WINDOW_W), px(WINDOW_H)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(WINDOW_MIN_W), px(WINDOW_H))),
            titlebar: Some(TitlebarOptions {
                title: Some("Harness".into()),
                ..TitleBar::window_options().titlebar.unwrap_or_default()
            }),
            ..TitleBar::window_options()
        };
        let stashed: std::rc::Rc<std::cell::RefCell<Option<gpui::Entity<bench::BenchRoot>>>> =
            std::rc::Rc::new(std::cell::RefCell::new(None));
        let capture_stash = stashed.clone();
        let handle = cx
            .open_window(options, |window, cx| {
                let view = cx.new(|cx| bench::BenchRoot::new(workspace.clone(), provider.clone(), window, cx));
                capture_stash.borrow_mut().replace(view.clone());
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("open the bench window");
        let Some(root) = stashed.borrow().clone() else {
            eprintln!("bench: the window closed before the run started");
            return;
        };
        handle
            .update(cx, |_, window, cx| {
                window.on_window_should_close(cx, |_, _| {
                    crate::tier::cleanup_probes();
                    true
                });
            })
            .ok();
        cx.on_app_quit(|_| async {
            crate::tier::cleanup_probes();
        })
        .detach();
        bench::run(handle, root, opts, command, cx);
    });
}

/// `~` at the start of a path, the one expansion a shell would have done.
fn shellexpand(path: &str) -> String {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => format!("{}/{rest}", home.to_string_lossy()),
        _ => path.to_owned(),
    }
}

fn main() {
    let args = parse_args();
    // The billing probe with no window: it drives the `muse` TUI in a pty,
    // prints the plan and leaves. Nothing here needs gpui.
    if args.print_tier {
        tier::print_and_exit(&args.program);
    }
    // The bench is its own window (one replayed session, streamed): it never
    // reaches the shell below.
    if args.bench.is_some() {
        run_bench(args);
        return;
    }
    let (theme, screenshot, delay) = (args.theme, args.screenshot.clone(), args.delay);
    // A `shell:` step raises a real approval over a live wire, which does not
    // land inside a fixed delay (finding F9).
    let await_approval = args.steps.iter().any(|step| step.starts_with("shell:"));
    let await_steps = !args.steps.is_empty() || !args.login_steps.is_empty();
    // 1. The asset source first: it serves `aui-icons` over gpui-kit's set.
    gpui_kit::application().with_assets(aui::assets::AuiAssets).run(move |cx| {
        // 2. One call does gpui_kit::init, the fonts, the themes and the keymap.
        aui::init(theme, cx);
        // A deterministic capture holds the platform's reduced-motion switch:
        // every motion primitive (loops, tweens, presence, springs, the
        // streaming caret) resolves to its resting state, so spinners and
        // shimmers with no `at_rest` of their own still paint one frame.
        if crate::clock::deterministic() {
            cx.set_reduce_motion(true);
        }
        // 3. The product text scale.
        AuiTheme::set_text_scale(scale::TEXT_SCALE, None, cx);
        // 4. The app's own keys, then the native menu bar: macOS reads each
        // menu item's shortcut from the keymap, so the menus come second.
        app::bind_keys(cx);
        app::set_menus(cx);

        let bounds = match screenshot {
            // A capture renders at the display's top-left, away from the
            // pointer, so no hover state leaks into the PNG.
            Some(_) => Bounds::new(gpui::point(px(0.0), px(0.0)), size(px(WINDOW_W), px(WINDOW_H))),
            None => Bounds::centered(None, size(px(WINDOW_W), px(WINDOW_H)), cx),
        };
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(WINDOW_MIN_W), px(WINDOW_MIN_H))),
            titlebar: Some(TitlebarOptions {
                title: Some("Harness".into()),
                ..TitleBar::window_options().titlebar.unwrap_or_default()
            }),
            ..TitleBar::window_options()
        };
        // One token per window: the capture's waits are this window's, not the
        // process's (finding `support-16`).
        let capture = shot::CaptureToken::default();
        let handle = cx
            .open_window(options, {
                let capture = capture.clone();
                move |window, cx| {
                let view = cx.new(|cx| app::Harness::new(args.clone(), capture.clone(), window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            }})
            .expect("open the harness window");
        // Closing the window — or quitting the app — with a tier probe still
        // driving the `muse` TUI would orphan it, the way quitting
        // mid-screenshot once did: the child is its own session leader, so
        // the `Pty` drop that would SIGKILL it never runs. Same kill plus
        // bounded wait as the screenshot path, on both hooks: macOS does not
        // quit when the last window closes, so the window hook covers the red
        // dot and the app hook covers Cmd+Q and `cx.quit()`. Each rerun is a
        // no-op once the probes are gone, and the quit proceeds when the wait
        // expires.
        handle.update(cx, |_, window, cx| {
            window.on_window_should_close(cx, |_, _| {
                crate::tier::cleanup_probes();
                true
            });
        }).ok();
        cx.on_app_quit(|_| async {
            crate::tier::cleanup_probes();
        })
        .detach();
        match screenshot {
            Some(path) => shot::capture_and_quit(handle, path, delay, await_steps, await_approval, capture, cx),
            None => cx.activate(true),
        }
    });
}
