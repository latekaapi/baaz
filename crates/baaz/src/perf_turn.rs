//! `--perf-turn`: what one streamed turn and one finished turn cost the UI
//! thread at the owner's sizes (docs/51 §10, speed S4), headless.
//!
//! Two measurements, each the code a live window runs, driven in-process
//! on gpui's test platform (no window on screen, no display, no child, no
//! model, a throwaway state dir):
//!
//! - **`render_turn_at_{300,3000}_us`**: one more turn streamed onto a
//!   300- and a 3,000-turn transcript, event by event, each folded into a
//!   [`SessionView`](crate::session::SessionView) and followed by the
//!   render-cache refresh a frame makes. The median per turn. The cost of a
//!   turn's arrival must not grow with the session.
//! - **`settle_ui_ms`**: a finished Muse turn's UI-thread work over 2,600
//!   sidebar rows — the index re-read's snapshot, the reply (rejoin, search
//!   rows) with one row's index entry moved, the row's counter and time
//!   moving to the top, and the next frame's visible list and grouping.
//!   Median.
//!
//! Prints one JSON object on stdout; `scripts/perf-budgets.py` reads it as
//! the `turn.*` metrics.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context};

use crate::app::Harness;
use crate::index::IndexEntry;
use crate::sidebar::SessionEntry;

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

/// The sidebar at the owner's size: 2,600 Muse rows and their index
/// entries (labels, workspaces across 12 projects, 2 KB of search text).
pub(crate) fn sidebar_fixture() -> (Vec<SessionEntry>, HashMap<String, IndexEntry>) {
    let base = crate::clock::now_local();
    let mut sessions = Vec::with_capacity(crate::perf_budget::SESSIONS);
    let mut index = HashMap::with_capacity(crate::perf_budget::SESSIONS);
    for i in 0..crate::perf_budget::SESSIONS {
        let id = format!("s-{i:05}");
        let workspace = format!("/work/project-{}", i % 12);
        sessions.push(SessionEntry {
            id: id.clone(),
            label: format!("Session {i}"),
            updated: base - chrono::Duration::minutes(i as i64),
            created: base - chrono::Duration::minutes(i as i64 + 5),
            running: false,
            turns: (i % 7) as u64 + 1,
            hidden: i % 23 == 0,
            pinned: i % 97 == 0,
            archived: false,
            description: String::new(),
            replayed: false,
            provider: crate::providers::ProviderId::Muse,
            named: false,
            needs_title: false,
            side_marker: false,
            title_pending: false,
            last_ask: None,
            local: false,
            provisional: false,
            turns_unknown: false,
            workspace: Some(workspace.clone()),
            project: None,
            project_name: None,
            attention: Vec::new(),
            approval_command: None,
            pending_question: None,
            turn_started: None,
            last_error: None,
            branch: None,
            terminals_running: 0,
        });
        index.insert(
            id,
            IndexEntry {
                title: format!("Session {i}"),
                first_user_prompt: Some(format!("Tighten validation round {i}")),
                search_text: format!("session {i} ").repeat(2048 / 12),
                updated_at_us: Some(1_790_000_000_000_000 - i as i64 * 60_000_000),
                workspace_root: Some(workspace),
                ..IndexEntry::default()
            },
        );
    }
    (sessions, index)
}

/// `n` finished turns, each in a different session: one index row moves
/// and the window pays what `load_index` and the next frame pay. Returns
/// the median per-settle milliseconds (and its parts).
pub(crate) fn time_settles(harness: &mut Harness, n: usize, cx: &mut Context<Harness>) -> serde_json::Value {
    let (mut snapshot, mut apply, mut visible, mut total) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for k in 0..n {
        let mut next = (*harness.index).clone();
        let id = format!("s-{:05}", (k * 37) % crate::perf_budget::SESSIONS);
        if let Some(entry) = next.get_mut(&id) {
            entry.updated_at_us = Some(1_800_000_000_000_000 + k as i64);
            entry.search_text.push_str(" freshword");
        }
        let next = std::sync::Arc::new(next);
        let at = Instant::now();
        // `load_index` snapshots the index on the UI thread before the read.
        let previous = std::sync::Arc::clone(&harness.index);
        let t_snapshot = at.elapsed();
        // What the background read hands back with the map (off the clock:
        // it runs on the background executor).
        let changed = crate::index::changed_ids(&previous, &next);
        drop(previous);
        let at_apply = Instant::now();
        harness.apply_index_delta(next, changed, cx);
        // The settled turn's own row: one more turn, at a newer time (what
        // `rejoin_provider_row` moves for a listed row) — it rises to the
        // top of the list.
        if let Some(ix) = harness.sessions.iter().position(|entry| entry.id == id) {
            let before = harness.sessions[ix].clone();
            harness.sessions[ix].turns += 1;
            harness.sessions[ix].updated = crate::clock::now_local() + chrono::Duration::seconds(k as i64 + 1);
            harness.invalidate_row(&before);
        }
        let t_apply = at_apply.elapsed();
        // The next frame: the visible list and the sidebar's grouping.
        let at_visible = Instant::now();
        let _ = harness.visible_sessions(cx).len();
        let _ = harness.sidebar_grouping(cx);
        let t_visible = at_visible.elapsed();
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        snapshot.push(ms(t_snapshot));
        apply.push(ms(t_apply));
        visible.push(ms(t_visible));
        total.push(ms(t_snapshot + t_apply + t_visible));
    }
    // The parts of `apply_index`, each timed alone on the settled state.
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let (mut rejoin, mut rows, mut list) = (Vec::new(), Vec::new(), Vec::new());
    for _ in 0..10 {
        let at = Instant::now();
        harness.rejoin();
        rejoin.push(ms(at.elapsed()));
        let at = Instant::now();
        let _ = harness.search_sync_rows().len();
        rows.push(ms(at.elapsed()));
        harness.invalidate_list();
        let at = Instant::now();
        let _ = harness.visible_sessions(cx).len();
        let _ = harness.sidebar_grouping(cx);
        list.push(ms(at.elapsed()));
    }
    serde_json::json!({
        "settle_part_rejoin_ms": median(rejoin),
        "settle_part_search_rows_ms": median(rows),
        "settle_part_list_rebuild_ms": median(list),
        "settle_ui_ms": median(total),
        "settle_snapshot_ms": median(snapshot),
        "settle_apply_index_ms": median(apply),
        "settle_visible_ms": median(visible),
    })
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
        let args = crate::Args::perf(dir.clone());
        let harness = cx.new(|cx| Harness::new(args, crate::shot::CaptureToken::default(), window, cx));
        harness.update(cx, |harness, cx| {
            let (sessions, index) = sidebar_fixture();
            harness.sessions = sessions;
            harness.index = std::sync::Arc::new(index);
            harness.sessions_loaded = true;
            harness.index_loaded = true;
            // The rows as a joined list holds them, so a settle compares
            // against real rows, not the fixture's raw ones.
            harness.rejoin();
            let _ = harness.visible_sessions(cx).len();
            let _ = harness.sidebar_grouping(cx);
            if let serde_json::Value::Object(settle) = time_settles(harness, 40, cx) {
                out.extend(settle);
            }
        });
    });
    let _ = std::fs::remove_dir_all(&dir);
    let value = serde_json::Value::Object(out);
    println!("{value}");
    value
}
