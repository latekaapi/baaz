//! Tests for listing: fixture rows, boot rows, search rows, row patches.

use super::*;

fn fixture_row() -> FixtureSession {
    serde_json::from_str(
        r#"{"sessionId": "s1", "workspaceRoot": "fixtures/ws/acme-web", "label": "Fix the header",
            "updatedAt": "2026-09-13T10:00:00Z", "turnCount": 3, "status": "running"}"#,
    )
    .expect("the documented fixture shape parses")
}

#[test]
fn a_fixture_row_joins_like_a_wire_row() {
    let row = fixture_row();
    assert_eq!(row.turn_count, 3);
    assert_eq!(row.status, "running");
    let launch = std::env::temp_dir().join(format!("baaz-fixture-{}", std::process::id()));
    let mut projects = Projects::default();
    let root = launch.join("fixtures/ws/acme-web");
    std::fs::create_dir_all(&root).expect("temp workspace");
    let id = projects.add(&root).id.clone();
    let entry = fixture_entry(&row, &launch, &projects);
    // The label is the fixture's own; the workspace and project are
    // what `join` resolved, exactly as for a listed session.
    assert_eq!(entry.id, "s1");
    assert_eq!(entry.label, "Fix the header");
    assert!(entry.running);
    assert_eq!(entry.turns, 3);
    assert_eq!(entry.project.as_deref(), Some(id.as_str()));
    // Kept like a replayed row's, so a later `rejoin` keeps it.
    assert!(entry.replayed);
    let _ = std::fs::remove_dir_all(&launch);
}

#[test]
fn an_absolute_root_survives_and_a_relative_one_joins_the_launch_dir() {
    let mut row = fixture_row();
    row.workspace_root = "/private/tmp/h4ws".into();
    let entry = fixture_entry(&row, std::path::Path::new("/repo"), &Projects::default());
    assert_eq!(entry.workspace.as_deref(), Some("/private/tmp/h4ws"));
    assert!(entry.project.is_none());
}

/// Perf P1: the search rows leave out side sessions by every mark —
/// the in-memory set, the override and the list's side marker — with
/// the marker read through one set per build, not a list scan per
/// indexed session (that scan was O(n²), ~30 ms per rebuild).
#[gpui::test]
fn search_rows_skip_side_sessions_by_every_mark(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("search-side-marks");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            for id in ["s-plain", "s-set", "s-meta", "s-marker"] {
                harness.session_list.update(cx, |list, _| std::sync::Arc::make_mut(&mut list.index).insert(id.into(), IndexEntry { title: id.into(), ..Default::default() }));
            }
            harness.session_list.update(cx, |list, _| list.side_sessions.insert("s-set".into()));
            harness.session_list.update(cx, |list, _| {
                list.overrides.insert(
                    "s-meta".into(),
                    crate::sessions::SessionMeta { side_session: true, ..Default::default() },
                );
            });
            let mut marked = chain_muse_entry("s-marker", "Marker", 1);
            marked.side_marker = true;
            harness.session_list.update(cx, |list, _| list.sessions.push(marked));
            let ids: Vec<String> = harness.search_session_rows(cx).into_iter().map(|row| row.session_id).collect();
            assert_eq!(ids, vec!["s-plain".to_owned()], "only the ordinary session is searchable");
        });
    });
    lane_restore(state);
}

/// Speed P1 (docs/61 §4a): the boot rows, the list reply and every
/// rejoin read one label ladder, so no search row changes as they take
/// turns. Before, a row whose wire title differed from its index label,
/// or whose label ran past 80 characters, flipped between the two and
/// rewrote its search row on every launch (63 rows on the owner's store).
#[gpui::test]
fn boot_rows_reply_and_rejoin_read_one_label(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("saved-list-boot-rows");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let wire: crate::list_cache::ListedRow = serde_json::from_value(serde_json::json!({
        "sessionId": "s-chain",
        "createdAt": "2026-10-01T00:00:00Z",
        "updatedAt": "2026-10-20T00:00:00Z",
        "lastActivityAt": "2026-10-20T00:00:00Z",
        "path": "/tmp/s-chain.jsonl",
        "status": "running",
        "turnCount": 3,
        "title": "Wire title",
        "activeTurnId": null, "forkedFrom": null, "modelId": null, "providerId": null,
        // Not a temp folder: a temp-folder run is scripted and never listed.
        "workspaceRoot": "/srv/w",
    }))
    .expect("a list row");
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.session_list.update(cx, |list, _| std::sync::Arc::make_mut(&mut list.index).insert(
                "s-chain".into(),
                IndexEntry { title: "Index title".into(), updated_at_us: Some(1), ..Default::default() },
            ));
            let reply = |harness: &mut Harness, cx: &mut gpui::App| {
                let joined = SessionEntry::join(&wire, harness.session_list.read(cx).index.get("s-chain"), None, &harness.session_list.read(cx).projects);
                harness.session_list.update(cx, |list, _| list.sessions = sidebar::merge_session_list(vec![joined], &list.sessions));
                harness.invalidate_list(cx);
                harness.search_sync_rows(cx).into_iter().map(|(row, _, fp)| (row.label, fp)).collect::<Vec<_>>()
            };
            // The index alone: the label moves when the reply lands.
            harness.install_provisional_rows(cx);
            let boot: Vec<_> =
                harness.search_sync_rows(cx).into_iter().map(|(row, _, fp)| (row.label, fp)).collect();
            assert_eq!(boot[0].0, "Index title");
            assert_ne!(boot, reply(harness, cx), "without the saved list the row is rewritten");
            // With the saved list: nothing moves.
            harness.session_list.update(cx, |list, _| list.sessions.clear());
            harness.session_list.update(cx, |list, _| list.fetch.saved = Some(std::sync::Arc::new(crate::list_cache::ListCache {
                sessions: vec![wire.clone()],
                ..Default::default()
            })));
            harness.install_provisional_rows(cx);
            let row = harness.session_list.read(cx).sessions.iter().find(|e| e.id == "s-chain").expect("a boot row");
            assert!(row.provisional && !row.running && !row.needs_title, "wire facts that may have moved stay zero");
            let boot: Vec<_> =
                harness.search_sync_rows(cx).into_iter().map(|(row, _, fp)| (row.label, fp)).collect();
            assert_eq!(boot[0].0, "Wire title");
            assert_eq!(boot, reply(harness, cx), "the reply changes no search row");
            // Nor does a rejoin after it (every index refresh and override
            // edit runs one): it used to read the index label, uncut.
            harness.rejoin(cx);
            let rejoined: Vec<_> =
                harness.search_sync_rows(cx).into_iter().map(|(row, _, fp)| (row.label, fp)).collect();
            assert_eq!(rejoined, boot, "a rejoin reads the same ladder as the join");
            // A label past 80 characters is cut the same way on both paths.
            let long = "word ".repeat(40);
            harness.session_list.update(cx, |list, _| std::sync::Arc::make_mut(&mut list.index).get_mut("s-chain").expect("indexed").title = long.clone());
            let mut untitled = wire.clone();
            untitled.title = None;
            harness.session_list.update(cx, |list, _| list.fetch.saved = Some(std::sync::Arc::new(crate::list_cache::ListCache {
                sessions: vec![untitled.clone()],
                ..Default::default()
            })));
            harness.rejoin(cx);
            let row = harness.session_list.read(cx).sessions.iter().find(|e| e.id == "s-chain").expect("a row");
            let joined = SessionEntry::join(&untitled, harness.session_list.read(cx).index.get("s-chain"), None, &harness.session_list.read(cx).projects);
            assert_eq!(row.label, joined.label, "rejoin and join agree on a long label");
            assert!(row.label.ends_with('\u{2026}'));
        });
    });
    lane_restore(state);
}

/// A second boot install (the index reconcile landing before the list
/// reply) keeps every row. It used to skip the rows the first install
/// placed and then drop them, leaving only local rows: 2,545 search rows
/// became 2,661 with every side session unhidden, and the sidebar held
/// almost nothing until the reply.
#[gpui::test]
fn a_second_boot_install_keeps_every_row(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("second-boot-install");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            for id in ["s-a", "s-b", "s-c"] {
                harness.session_list.update(cx, |list, _| std::sync::Arc::make_mut(&mut list.index).insert(id.into(), IndexEntry { title: id.into(), ..Default::default() }));
            }
            harness.install_provisional_rows(cx);
            let ids = |h: &Harness, cx: &gpui::App| {
                let mut ids: Vec<String> = h.session_list.read(cx).sessions.iter().filter(|e| e.provisional).map(|e| e.id.clone()).collect();
                ids.sort();
                ids
            };
            assert_eq!(ids(harness, cx), ["s-a", "s-b", "s-c"]);
            // The reconcile brings one more row.
            harness.session_list.update(cx, |list, _| std::sync::Arc::make_mut(&mut list.index).insert("s-d".into(), IndexEntry { title: "s-d".into(), ..Default::default() }));
            harness.install_provisional_rows(cx);
            assert_eq!(ids(harness, cx), ["s-a", "s-b", "s-c", "s-d"], "nothing drops, the new row joins");
        });
    });
    lane_restore(state);
}

/// The boot rows paint before the list reply (docs/51 P0 #2): the
/// skeleton stays only until the index lands, or while there is no row
/// to show. The D3 screen waited for `session/list` again.
#[gpui::test]
fn the_sidebar_paints_boot_rows_before_the_list_reply(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("boot-rows-paint");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.session_list.update(cx, |list, _| list.sessions.retain(|e| e.local));
            assert!(harness.session_list.read(cx).loading(), "nothing has landed");
            harness.session_list.update(cx, |list, _| list.index_loaded = true);
            assert!(harness.session_list.read(cx).loading(), "the index landed empty: still nothing to paint");
            harness.session_list.update(cx, |list, _| std::sync::Arc::make_mut(&mut list.index).insert("s-a".into(), IndexEntry { title: "A".into(), ..Default::default() }));
            harness.install_provisional_rows(cx);
            assert!(!harness.session_list.read(cx).sessions_loaded, "the wire has not answered");
            assert!(!harness.session_list.read(cx).loading(), "boot rows paint without waiting for the list");
            // A zero-turn session from the saved list is filtered as empty
            // now, as the reply's row will be; an index-only row, whose turn
            // count nobody knows yet, still shows.
            let zero: crate::list_cache::ListedRow = serde_json::from_value(serde_json::json!({
                "sessionId": "s-zero", "createdAt": "2026-10-01T00:00:00Z",
                "updatedAt": "2026-10-20T00:00:00Z", "path": "/tmp/z.jsonl", "status": "idle",
                "turnCount": 0, "title": "Zero", "activeTurnId": null, "forkedFrom": null,
                "modelId": null, "providerId": null, "workspaceRoot": "/tmp/w",
            }))
            .expect("a list row");
            harness.session_list.update(cx, |list, _| list.fetch.saved = Some(std::sync::Arc::new(crate::list_cache::ListCache {
                sessions: vec![zero],
                ..Default::default()
            })));
            harness.install_provisional_rows(cx);
            let visible: Vec<String> = harness.visible_sessions(cx).iter().map(|e| e.id.clone()).collect();
            assert!(visible.contains(&"s-a".to_owned()), "the index-only row shows");
            assert!(!visible.contains(&"s-zero".to_owned()), "a known-empty row is filtered before the reply too");
        });
    });
    lane_restore(state);
}

/// `visible_sessions` (docs/51 P2): the sidebar's filtered, collapsed,
/// sorted rows over 2,600 sessions — rebuilt after any list change (a
/// finished turn, a rename, a pin) and served from its cache otherwise.
/// A criterion run from an ignored test (baaz is a binary crate; see
/// `perf_budget::bench_criterion`).
#[gpui::test]
#[ignore = "bench: run with --ignored --nocapture"]
fn bench_visible_sessions(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("bench-visible-sessions");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let base = crate::clock::now_local();
            let rows = (0..crate::perf_budget::SESSIONS)
                .map(|i| {
                    let mut entry = chain_muse_entry(&format!("s-{i:05}"), &format!("Session {i}"), (i % 7) as u64);
                    entry.updated = base - chrono::Duration::minutes(i as i64);
                    entry.hidden = i % 23 == 0;
                    entry.pinned = i % 97 == 0;
                    entry.workspace = Some(format!("/work/project-{}", i % 12));
                    entry
                })
                .collect();
            harness.session_list.update(cx, |list, _| list.sessions = rows);
            harness.session_list.update(cx, |list, _| list.sessions_loaded = true);
            harness.session_list.update(cx, |list, _| list.index_loaded = true);
            harness.invalidate_list(cx);
            let shown = harness.visible_sessions(cx).len();
            assert!(shown > 2_000, "the filter keeps the non-empty, unhidden rows: {shown}");
            let mut c = crate::perf_budget::bench_criterion();
            c.bench_function("visible_sessions/2600_after_a_change", |b| {
                b.iter(|| {
                    harness.invalidate_list(cx);
                    harness.visible_sessions(cx).len()
                })
            });
            c.bench_function("visible_sessions/2600_cached", |b| b.iter(|| harness.visible_sessions(cx).len()));
            c.final_summary();
        });
    });
    lane_restore(state);
}

/// Speed S4 #2 (docs/51 §10): the UI-thread work one finished Muse turn
/// costs the window at the owner's size — the index re-read's snapshot,
/// the reply's `apply_index` (rejoin + search rows) and the next
/// frame's `visible_sessions` rebuild. Prints each part.
#[gpui::test]
#[ignore = "bench: run with --ignored --nocapture"]
fn bench_turn_settle(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("bench-turn-settle");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let (sessions, index) = crate::perf_turn::sidebar_fixture();
            harness.session_list.update(cx, |list, _| list.sessions = sessions);
            harness.session_list.update(cx, |list, _| list.index = std::sync::Arc::new(index));
            harness.session_list.update(cx, |list, _| list.sessions_loaded = true);
            harness.session_list.update(cx, |list, _| list.index_loaded = true);
            harness.rejoin(cx);
            let _ = harness.visible_sessions(cx).len();
            let report = crate::perf_turn::time_settles(harness, 40, cx);
            eprintln!("bench_turn_settle {report}");
        });
    });
    lane_restore(state);
}

/// Speed S4: a row whose time, turns or words moved is patched into the
/// cached list in place — and the list it leaves is exactly the one a
/// whole rebuild sorts. A row that changes what the list filters on
/// still rebuilds.
#[gpui::test]
fn a_patched_row_leaves_the_list_a_rebuild_would(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("patched-row");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let (sessions, index) = crate::perf_turn::sidebar_fixture();
            harness.session_list.update(cx, |list, _| list.sessions = sessions);
            harness.session_list.update(cx, |list, _| list.index = std::sync::Arc::new(index));
            harness.session_list.update(cx, |list, _| list.sessions_loaded = true);
            harness.session_list.update(cx, |list, _| list.index_loaded = true);
            harness.rejoin(cx);
            let _ = harness.visible_sessions(cx);
            for (k, id) in ["s-01000", "s-00001", "s-02500"].iter().enumerate() {
                let ix = harness.session_list.read(cx).sessions.iter().position(|entry| entry.id == *id).expect("fixture row");
                let before = harness.session_list.read(cx).sessions[ix].clone();
                let epoch = harness.session_list.read(cx).epoch;
                harness.session_list.update(cx, |list, _| list.sessions[ix].turns += 1);
                harness.session_list.update(cx, |list, _| list.sessions[ix].running = k == 1);
                harness.session_list.update(cx, |list, _| list.sessions[ix].description = format!("moved {k}"));
                harness.session_list.update(cx, |list, _| list.sessions[ix].updated = crate::clock::now_local() + chrono::Duration::minutes(k as i64 + 1));
                harness.invalidate_row(&before, cx);
                assert_eq!(harness.session_list.read(cx).epoch, epoch, "a time-and-words move patches, never rebuilds");
                let patched = harness.visible_sessions(cx).as_ref().clone();
                assert_eq!(patched[0].id, *id, "the moved row leads");
                harness.invalidate_list(cx);
                let rebuilt = harness.visible_sessions(cx).as_ref().clone();
                assert_eq!(patched, rebuilt, "row {id}");
            }
            let ix = harness.session_list.read(cx).sessions.iter().position(|entry| entry.id == "s-00005").expect("fixture row");
            let before = harness.session_list.read(cx).sessions[ix].clone();
            harness.session_list.update(cx, |list, _| list.sessions[ix].hidden = !before.hidden);
            let epoch = harness.session_list.read(cx).epoch;
            harness.invalidate_row(&before, cx);
            assert_ne!(harness.session_list.read(cx).epoch, epoch, "a filtered field rebuilds");
        });
    });
    lane_restore(state);
}

/// Speed S4: one session's search row, built alone, is the row the
/// whole build makes for it; a chained or unknown id cannot stand alone.
#[gpui::test]
fn a_scoped_search_row_is_the_whole_build_s_row(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("scoped-search-row");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let (sessions, index) = crate::perf_turn::sidebar_fixture();
            harness.session_list.update(cx, |list, _| list.sessions = sessions);
            harness.session_list.update(cx, |list, _| list.index = std::sync::Arc::new(index));
            harness.session_list.update(cx, |list, _| list.sessions_loaded = true);
            harness.session_list.update(cx, |list, _| list.index_loaded = true);
            harness.rejoin(cx);
            let full = harness.search_sync_rows(cx);
            for id in ["s-00042", "s-01999"] {
                let ids: std::collections::HashSet<String> = std::iter::once(id.to_owned()).collect();
                let scoped = harness.search_rows_scoped(Some(&ids), cx).expect("a plain row stands alone");
                let whole = full.iter().find(|(row, _, _)| row.session_id == id).expect("in the whole build");
                assert_eq!(scoped.len(), 1);
                let words = |(row, body_ids, fingerprint): &(crate::search::SessionRow, Vec<String>, i64)| {
                    (
                        row.session_id.clone(),
                        row.label.clone(),
                        row.title.clone(),
                        row.first_prompt.clone(),
                        row.body.clone(),
                        row.project.clone(),
                        row.workspace.clone(),
                        body_ids.clone(),
                        *fingerprint,
                    )
                };
                assert_eq!(words(&scoped[0]), words(whole), "{id}");
            }
            let unknown: std::collections::HashSet<String> = std::iter::once("s-nowhere".to_owned()).collect();
            assert!(harness.search_rows_scoped(Some(&unknown), cx).is_none(), "an id in neither store rebuilds everything");
        });
    });
    // A finished turn's index reply and one session's override edit
    // each sync that session's search row alone.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let mut next = (*harness.session_list.read(cx).index).clone();
            next.get_mut("s-00042").expect("fixture row").search_text.push_str(" freshword");
            let next = std::sync::Arc::new(next);
            let changed = crate::index::changed_ids(&harness.session_list.read(cx).index, &next);
            assert_eq!(changed.as_ref().map(|ids| ids.len()), Some(1), "one row moved");
            Harness::reset_search_rebuild_count();
            harness.apply_index_delta(next, changed, cx);
            assert_eq!(Harness::search_rebuild_count(), 1);
            assert_eq!(Harness::search_scoped_rebuild_count(), 1, "the index reply syncs one row");
            Harness::reset_search_rebuild_count();
            harness.set_override("s-00043", |meta| meta.last_ask = Some("what changed?".into()), cx);
            assert_eq!(Harness::search_scoped_rebuild_count(), 1, "one session's override syncs one row");
        });
    });
    lane_restore(state);
}

/// Perf P1: an override edit that changes nothing settles nothing. A
/// provider reopen re-states the project its session already has, and
/// that used to rejoin the list and rebuild the search rows — 75 ms of
/// UI thread per reopen. A real change still settles.
#[gpui::test]
fn an_override_edit_that_changes_nothing_settles_nothing(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("override-noop");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            // Indexed, so the edit is not the inert new-session case.
            harness.session_list.update(cx, |list, _| std::sync::Arc::make_mut(&mut list.index).insert("s-known".into(), IndexEntry { title: "Known".into(), ..Default::default() }));
            Harness::reset_search_rebuild_count();
            harness.set_override("s-known", |meta| meta.project = Some("p-1".into()), cx);
            assert_eq!(Harness::search_rebuild_count(), 1, "a real change settles");
            Harness::reset_search_rebuild_count();
            harness.set_override("s-known", |meta| meta.project = Some("p-1".into()), cx);
            assert_eq!(Harness::search_rebuild_count(), 0, "the same project again settles nothing");
            assert_eq!(harness.session_list.read(cx).overrides.get("s-known").and_then(|meta| meta.project.clone()), Some("p-1".into()));
        });
    });
    lane_restore(state);
}

/// B7fix2: the handoff destination inherits pane state through the
/// cheap save — no row rejoin, no search-index rebuild, no sync
/// settle write. The restore that follows flushes the armed write, so
/// the carried state still reaches the disk at once.
#[gpui::test]
fn handoff_destination_pane_state_skips_the_list_settle(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("handoff-carry-cheap");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let carried = crate::sessions::RightState {
        open: true,
        // B8b: every load AND write uses `changes` — the carried kind
        // is `Changes` (the legacy `diff`/`git` slugs load as `Changes`
        // and re-save as `changes`).
        kind: crate::layout::RightKind::Changes,
        ..Default::default()
    };
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.session_list.update(cx, |list, _| {
                list.overrides.insert(
                    "source-1".to_owned(),
                    crate::sessions::SessionMeta {
                        right: Some(carried.clone()),
                        ..Default::default()
                    },
                );
            });
            Harness::reset_search_rebuild_count();
            harness.carry_right_state_for_handoff("dest-1", carried.clone(), cx);
            assert_eq!(
                harness.session_list.read(cx).overrides.get("dest-1").and_then(|meta| meta.right.clone()),
                Some(carried.clone()),
                "the destination inherits the source's pane state"
            );
            assert_eq!(
                Harness::search_rebuild_count(),
                0,
                "the handoff carry never settles the session list"
            );
        });
    });
    assert_eq!(
        crate::sessions::read().get("dest-1").and_then(|meta| meta.right.clone()),
        Some(carried),
        "the restore flushes the carried state to disk at once"
    );
    lane_restore(state);
}

/// docs/50 P1: with Muse unavailable the sidebar settles on the provider
/// records at once, and a Muse sign-out takes only Muse's rows.
#[gpui::test]
fn muse_never_gates_the_other_providers_rows(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("p1-ungate");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.session_list.update(cx, |list, _| list.sessions_loaded = false);
            harness.session_list.update(cx, |list, _| list.sessions.clear());
            z4_seed_stored_codex(harness, cx);
            harness.session_list.update(cx, |list, _| list.sessions.retain(|entry| !entry.provider.has(provider::ProviderTrait::NativeSessionList)));
            harness.session_list.update(cx, |list, _| list.sessions_loaded = false);
            harness.muse_unavailable(crate::connect::MUSE_OFF.into(), cx);
            assert!(harness.session_list.read(cx).sessions_loaded, "the sidebar settles without a Muse list");
            assert!(harness.session_list.read(cx).sessions.iter().any(|e| e.id == "s-codex-old"), "the Codex row shows");
            assert!(!matches!(harness.wire, crate::app::Wire::Ready));
            // A Muse row beside it, then a Muse sign-out.
            let mut muse_row = harness.session_list.read(cx).sessions[0].clone();
            muse_row.id = "s-muse".into();
            muse_row.provider = crate::providers::ProviderId::Muse;
            harness.session_list.update(cx, |list, _| list.sessions.push(muse_row));
            harness.forget_muse_rows(cx);
            assert!(harness.session_list.read(cx).sessions.iter().all(|e| e.id != "s-muse"), "Muse's row goes");
            assert!(harness.session_list.read(cx).sessions.iter().any(|e| e.id == "s-codex-old"), "Codex's row stays");
            // Offline chrome boots into the capture login screen; a live
            // launch with Muse signed out does not.
            harness.args.offline = false;
            assert!(harness.shell_up(cx), "the shell is up in live mode with Muse signed out");
            harness.args.offline = true;
        });
    });
    lane_restore(state);
}

/// docs/50 P2: a Codex session's title comes from Codex (through the
/// one-shot completer), never from a Muse side session.
#[gpui::test]
fn a_provider_sessions_title_comes_from_its_own_provider(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("p2-title");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::<crate::providers::ProviderId>::new()));
    {
        let asked = asked.clone();
        crate::utility::set_test_completer(move |provider, _| {
            asked.borrow_mut().push(provider);
            Ok("Parser Panic Fix".into())
        });
    }
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            mark_muse_up(harness);
            z4_seed_stored_codex(harness, cx);
            harness.session_list.update(cx, |list, _| {
                if let Some(record) = list.provider_sessions.get_mut("s-codex-old") {
                    record.turns = 0;
                }
            });
            harness.session_list.update(cx, |list, _| {
                for entry in list.sessions.iter_mut().filter(|e| e.id == "s-codex-old") {
                    entry.turns = 0;
                }
            });
            crate::layout::Layout::stage(cx, |layout| layout.auto_title = true);
            harness.maybe_start_title("s-codex-old", Some("why does the parser panic".into()), cx);
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert_eq!(*asked.borrow(), vec![crate::providers::ProviderId::Codex], "Codex wrote it, once");
        assert_eq!(
            harness.session_list.read(cx).overrides.get("s-codex-old").and_then(|m| m.generated_title.clone()).as_deref(),
            Some("Parser Panic Fix")
        );
    });
    lane_restore(state);
}

/// P4: a Muse session's title takes the same one-shot path as the
/// others — the completer is asked for Muse, and the side session it
/// rides is recorded and hidden before anything is sent.
#[gpui::test]
fn a_muse_sessions_title_rides_a_recorded_hidden_side_session(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("p4-muse-title");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let log = state.2.join("requests.log");
    let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::<crate::providers::ProviderId>::new()));
    {
        let asked = asked.clone();
        crate::utility::set_test_completer(move |provider, _| {
            asked.borrow_mut().push(provider);
            Ok("Ferry Route Chart".into())
        });
    }
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            install_fake_muse(harness, &state.2, &state.2, &log, None);
            harness.session_list.update(cx, |list, _| {
                list.sessions.push(crate::sidebar::local_started_row(
                    "s-muse-new",
                    "chart the ferry routes".into(),
                    None,
                    None,
                    chrono::Local::now(),
                ));
            });
            crate::layout::Layout::stage(cx, |layout| layout.auto_title = true);
            harness.maybe_start_title("s-muse-new", Some("chart the ferry routes".into()), cx);
            assert_eq!(harness.session_list.read(cx).side_sessions.len(), 1, "the side session is recorded before the call");
            let side = harness.session_list.read(cx).side_sessions.iter().next().cloned().expect("a side id");
            assert!(harness.session_list.read(cx).overrides.get(&side).is_some_and(|meta| meta.hidden && meta.side_session));
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert_eq!(*asked.borrow(), vec![crate::providers::ProviderId::Muse], "Muse wrote it, once");
        assert_eq!(
            harness.session_list.read(cx).overrides.get("s-muse-new").and_then(|m| m.generated_title.clone()).as_deref(),
            Some("Ferry Route Chart")
        );
    });
    lane_restore(state);
}
