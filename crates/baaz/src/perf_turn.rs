//! `--perf-turn`: what one streamed turn costs the UI thread at the
//! owner's sizes (docs/51 §10, speed S4), headless.
//!
//! One measurement, the code a live window runs, driven in-process
//! on gpui's test platform (no window on screen, no display, no child, no
//! model, a throwaway state dir):
//!
//! - **`render_turn_at_{300,3000}_us`**: one more turn streamed onto a
//!   300- and a 3,000-turn transcript, event by event, each folded into a
//!   [`SessionView`](crate::session::SessionView) and followed by the
//!   render-cache refresh a frame makes. The median per turn. The cost of a
//!   turn's arrival must not grow with the session.
//!
//! Prints one JSON object on stdout; `scripts/perf-budgets.py` reads it as
//! the `turn.*` metrics.

use std::time::Instant;

use gpui::AppContext as _;

/// One fixture capture's `<--` bodies.
fn fixture_lines(name: &str) -> Vec<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/msp").join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        // A bundle runs away from the checkout: the capture beside the
        // working directory, as `--bench` reads it.
        std::fs::read_to_string(std::path::Path::new("fixtures/msp").join(name)).unwrap_or_default()
    });
    text.lines().filter_map(|line| line.strip_prefix("<-- ")).map(str::to_owned).collect()
}

/// `lines` with every id the fold keys on renamed for turn `n`.
fn renamed(lines: &[String], from: &str, n: usize, j0: usize) -> Vec<String> {
    lines
        .iter()
        .enumerate()
        .map(|(j, body)| {
            let mut body = body
                .replace(from, &format!("t-perf-{n}"))
                .replace("i-stress-", &format!("i-perf-{n}-"))
                .replace("call_stress_300", &format!("call_perf_{n}"));
            if let Some(at) = body.find("\"v:stress:") {
                let end = body[at + 1..].find('"').map(|e| at + 1 + e).unwrap_or(at);
                body.replace_range(at..=end, &format!("\"v:perf:{n}:{}\"", j0 + j));
            }
            body
        })
        .collect()
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    values.get(values.len() / 2).copied().unwrap_or(0.0)
}

/// A fresh session view for the stress capture, on `window`.
fn stress_view(window: &mut gpui::Window, cx: &mut gpui::App) -> gpui::Entity<crate::session::SessionView> {
    let host = crate::session::SessionHost {
        provider_id: "meta".to_owned(),
        workspace: "/tmp/perf-turn".to_owned(),
        overlays: cx.new(|_| crate::overlays::Overlays::default()),
        capture: crate::shot::CaptureToken::default(),
        terminal_host: None,
    };
    cx.new(|cx| crate::session::SessionView::new("synthetic-stress-300".to_owned(), host, window, cx))
}

/// The per-turn render-cache cost at `extra` turns past the capture's 300.
fn time_render_turns(
    capture: &[String],
    last_turn: &[String],
    extra: usize,
    reps: usize,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) -> f64 {
    let mut folder = provider_muse::replay::Folder::new();
    let view = stress_view(window, cx);
    let events: Vec<_> = capture.iter().flat_map(|body| folder.fold_line(body)).collect();
    view.update(cx, |view, cx| {
        view.fold_provider_events(events, cx);
        for n in 0..extra {
            for body in renamed(last_turn, "t-stress-300", n, 0) {
                view.fold_provider_events(folder.fold_line(&body), cx);
            }
        }
        view.refresh_render_cache();
    });
    let base = view.read(cx).cached_turn_count();
    assert!(base >= 300 + extra, "the capture and {extra} more turns folded: {base}");
    let mut per_turn = Vec::with_capacity(reps);
    for rep in 0..reps {
        // Folded off the clock: the capture's own translation is not the
        // window's cost.
        let turn: Vec<Vec<_>> = renamed(last_turn, "t-stress-300", extra + rep, 0)
            .iter()
            .map(|body| folder.fold_line(body))
            .collect();
        let at = Instant::now();
        view.update(cx, |view, cx| {
            for events in turn {
                view.fold_provider_events(events, cx);
                view.refresh_render_cache();
            }
        });
        per_turn.push(at.elapsed().as_secs_f64() * 1e6);
    }
    assert!(view.read(cx).cached_turn_count() >= base + reps, "the timed turns landed");
    median(per_turn)
}

/// Run every measurement, print one JSON object, return it.
pub fn run() -> serde_json::Value {
    let dir = std::env::temp_dir().join(format!("baaz-perf-turn-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    if std::env::var_os("BAAZ_STATE_DIR").is_none() {
        std::env::set_var("BAAZ_STATE_DIR", &dir);
    }
    let mut cx = gpui::TestAppContext::single();
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let capture = fixture_lines("synthetic-stress-300.jsonl");
    assert!(!capture.is_empty(), "perf-turn: fixtures/msp/synthetic-stress-300.jsonl is readable");
    let last_turn: Vec<String> = capture.iter().filter(|body| body.contains("\"t-stress-300\"")).cloned().collect();
    let mut out = serde_json::Map::new();
    let vc = cx.add_empty_window();
    vc.update(|window, cx| {
        let at_300 = time_render_turns(&capture, &last_turn, 0, 200, window, cx);
        let at_3000 = time_render_turns(&capture, &last_turn, 2_700, 200, window, cx);
        out.insert("render_turn_at_300_us".into(), serde_json::json!(at_300));
        out.insert("render_turn_at_3000_us".into(), serde_json::json!(at_3000));
    });
    let _ = std::fs::remove_dir_all(&dir);
    let value = serde_json::Value::Object(out);
    println!("{value}");
    value
}
