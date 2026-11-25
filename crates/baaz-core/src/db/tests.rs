//! The database's own tests: the first-launch import from real-shaped JSON
//! stores, the window's two views of the session rows, and forgetting a
//! session. Every test works in a temp dir or in memory; none names the
//! real state dir.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

use super::json_import::{self, Step};
use super::sessions::{self as rows, FrontState};
use crate::provider_sessions::ProviderSessionStore;
use crate::sessions::{Overrides, SessionMeta};

/// A fresh temp dir for one test.
fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("baaz-db-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn write(dir: &Path, name: &str, text: &str) {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("parent");
    }
    std::fs::write(path, text).expect("fixture");
}

/// Today's stores, written by hand in their real shape: three providers,
/// one id two providers claim (D-10: the records say Codex, the import
/// journal says Claude Code), a renamed and an archived session, a pinned
/// one, a hand-off between two of them, display texts, prompt history in
/// two workspaces, a session's sent turns with a picture (two with the same
/// text), an import journal and a migration journal mid-move.
fn real_shaped_stores(dir: &Path) {
    write(
        dir,
        "session-meta.json",
        r#"{
  "c-1": {"name": "Renamed by hand", "pinned": true, "project": "p-shop", "handoffTo": "x-shared"},
  "m-solo": {"archived": true, "project": null},
  "x-shared": {"project": "p-shop"},
  "side-1": {"hidden": true, "sideSession": true}
}"#,
    );
    write(
        dir,
        "session-cache.json",
        r#"{
  "c-1": {"generatedTitle": "Fix the header", "lastAsk": "Make it blue", "lastSummary": "Done.", "right": {"open": true, "kind": "changes", "filesExpanded": ["src"]}},
  "m-solo": {"derivedTitle": "Old muse chat", "unread": true},
  "x-shared": {"titleAttempted": true, "lastError": "The model refused"}
}"#,
    );
    write(
        dir,
        "provider-sessions.json",
        r#"{
  "c-1": {"provider": "claude-code", "sessionId": "c-1", "workspace": "/work/shop", "project": "p-shop", "createdMs": 1790000000000, "updatedMs": 1790000500000, "turns": 4, "title": "Ack title", "firstPrompt": "Fix the header please", "displayTexts": {"Continuing a session handed off from Muse.\n## Goal": "Handed off from Muse: Goal"}},
  "x-shared": {"provider": "codex", "sessionId": "x-shared", "workspace": "/work/shop", "createdMs": 1790000600000, "updatedMs": 1790000700000, "turns": 2, "handoffFrom": "c-1", "handoffFromProvider": "claude-code", "handoffTitle": "Fix the header"},
  "m-1": {"provider": "muse", "sessionId": "m-1", "createdMs": 1789000000000, "updatedMs": 1789000000001, "turns": 7, "title": "A muse session"},
  "c-imp": {"provider": "claude-code", "sessionId": "c-imp", "workspace": "/work/old", "createdMs": 1780000000000, "updatedMs": 1780000000001, "turns": 3, "firstPrompt": "Imported words", "origin": "imported"}
}"#,
    );
    write(
        dir,
        "history.json",
        r#"{"/work/shop": ["first", "second", "third"], "/work/old": ["only"]}"#,
    );
    write(
        dir,
        "sent/c-1.json",
        r#"[{"text":"same","attachments":[{"name":"one.png","kind":"image","size_bytes":3,"meta":null,"state":{"kind":"ready"},"path":"PICTURE","quote_of":null}]},{"text":"same","attachments":[{"name":"notes.txt","kind":"file","size_bytes":5,"meta":null,"state":{"kind":"ready"},"path":"/work/shop/notes.txt","quote_of":null}],"annotations":{"screenshot":null,"size":[800.0,600.0],"address":"example.com/","notes":[{"index":1,"at":[10.0,20.0],"selector":"p","note":"bigger"}]}}]"#
            .replace("PICTURE", &dir.join("sent/c-1/a1.png").to_string_lossy())
            .as_str(),
    );
    write(dir, "sent/c-1/a1.png", "png");
    write(
        dir,
        "import-journal.json",
        r#"{"runs": [{"id": "import-1790000000000", "atMs": 1790000000000, "sessions": [{"provider": "claude-code", "sessionId": "c-imp"}, {"provider": "claude-code", "sessionId": "x-shared"}]}]}"#,
    );
    write(
        dir,
        "session-migration-journal.json",
        r#"{"completed": {"claude-code/projects/-work/c-1.jsonl": {"at": 1790000000, "outcome": "copied"}}, "in_progress": {"codex/sessions/2026/10/02/rollout-x.jsonl": {"at": 1790000100}}}"#,
    );
}

/// Every file under `dir` with its bytes, the database's own aside.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).expect("read dir").flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(super::FILE_NAME) {
                continue;
            }
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.insert(path.clone(), std::fs::read(&path).expect("read"));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, &mut out);
    out
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0)).expect("count")
}

fn uuid_of(conn: &Connection, provider: &str, id: &str) -> Option<String> {
    rows::exact_row(conn, provider, id).expect("query")
}

/// docs/82 4.12, the plan's "done when": the import from a copy of
/// real-shaped stores lands every fact, leaves every JSON file byte for
/// byte as it was, and a second launch imports nothing.
#[test]
fn the_first_launch_imports_every_store_and_leaves_the_json_untouched() {
    let dir = temp("import");
    real_shaped_stores(&dir);
    let before = snapshot(&dir);
    let mtimes: Vec<_> = before.keys().map(|path| std::fs::metadata(path).and_then(|meta| meta.modified()).ok()).collect();

    let path = super::path_in(&dir);
    let conn = super::as_writer(|| super::open_at(&path)).expect("the database opens and imports");

    // Sessions: one row each, keyed by provider and id.
    assert_eq!(count(&conn, "session"), 6, "c-1, x-shared, m-1, c-imp, m-solo, side-1");
    assert!(uuid_of(&conn, "claude-code", "c-1").is_some());
    assert!(uuid_of(&conn, "codex", "x-shared").is_some());
    assert!(uuid_of(&conn, "muse", "m-1").is_some());
    assert!(uuid_of(&conn, "", "m-solo").is_some(), "a session with no record has no provider yet");
    let (facts, records) = rows::load_front(&conn, &mut FrontState::default()).expect("load");
    // The renamed, pinned session: the person's facts and the cache's, and
    // the record's title, first prompt and project, on one row.
    let c1 = &facts["c-1"];
    assert_eq!(c1.name.as_deref(), Some("Renamed by hand"));
    assert!(c1.pinned);
    assert_eq!(c1.project.as_deref(), Some("p-shop"));
    assert_eq!(c1.generated_title.as_deref(), Some("Fix the header"));
    assert_eq!(c1.provider_title.as_deref(), Some("Ack title"));
    assert_eq!(c1.first_prompt.as_deref(), Some("Fix the header please"));
    assert_eq!(c1.last_ask.as_deref(), Some("Make it blue"));
    assert_eq!(c1.handoff_to.as_deref(), Some("x-shared"));
    let right = c1.right.as_ref().expect("the pane state");
    assert!(right.open && right.kind == crate::layout::RightKind::Changes && right.files_expanded == ["src"]);
    assert_eq!(c1.title(), Some("Renamed by hand"), "the title is the row's own");
    // The hand-off destination: its record's links and chain title.
    let shared = &facts["x-shared"];
    assert_eq!(shared.handoff_from.as_deref(), Some("c-1"));
    assert_eq!(shared.handoff_from_provider.as_deref(), Some("claude-code"));
    assert_eq!(shared.handoff_title.as_deref(), Some("Fix the header"));
    assert!(shared.title_attempted);
    assert_eq!(shared.last_error.as_deref(), Some("The model refused"));
    // The archived Muse session with no record, and a side session.
    assert!(facts["m-solo"].archived && facts["m-solo"].unread);
    assert_eq!(facts["m-solo"].title(), Some("Old muse chat"));
    assert!(facts["side-1"].hidden && facts["side-1"].side_session);
    // Records: provider, workspace, times, turns, origin.
    assert_eq!(records.len(), 4);
    assert_eq!(records["c-1"].provider, "claude-code");
    assert_eq!(records["c-1"].workspace.as_deref(), Some("/work/shop"));
    assert_eq!((records["c-1"].created_ms, records["c-1"].updated_ms, records["c-1"].turns), (1790000000000, 1790000500000, 4));
    assert_eq!(records["x-shared"].provider, "codex");
    assert!(records["c-imp"].is_imported());
    assert_eq!(facts["c-imp"].first_prompt.as_deref(), Some("Imported words"));
    assert_eq!(records["m-1"].turns, 7);
    assert_eq!(facts["m-1"].provider_title.as_deref(), Some("A muse session"));
    assert!(!records.contains_key("m-solo"));

    // The bubble text a submit showed, keyed by the echoed text (no turn id
    // was ever kept for it).
    let displays = super::turns::displays(&conn, "claude-code", "c-1").expect("displays");
    assert_eq!(
        displays.by_text.get("Continuing a session handed off from Muse.\n## Goal").map(String::as_str),
        Some("Handed off from Muse: Goal")
    );
    // Prompt history, oldest first.
    assert_eq!(super::history::read(&conn, "/work/shop").unwrap(), ["first", "second", "third"]);
    assert_eq!(super::history::read(&conn, "/work/old").unwrap(), ["only"]);
    // Sent turns: both, in order, the picture's path kept relative too.
    let sent = super::attachments::load(&conn, "claude-code", "c-1").expect("sent");
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].attachments[0].name, "one.png");
    assert_eq!(sent[1].attachments[0].name, "notes.txt");
    assert_eq!(sent[1].annotations.as_ref().map(|batch| batch.notes[0].note.as_str()), Some("bigger"));
    assert!(sent.iter().all(|item| item.turn_id.is_none()), "imported turns pair by text");
    let rel: String = conn
        .query_row("SELECT file_rel_path FROM attachment WHERE position = 0 AND ordinal = 0", [], |row| row.get(0))
        .expect("relative path");
    assert_eq!(rel, "sent/c-1/a1.png");
    // The import journal keeps the provider each item named: the shared id
    // is Claude Code's in the journal and Codex's in the records (D-10), and
    // neither answers for the other.
    let journal = super::journals::read_import(&conn).expect("journal");
    assert_eq!(journal.runs.len(), 1);
    assert_eq!(journal.runs[0].sessions[1].provider, "claude-code");
    assert_eq!(journal.runs[0].sessions[1].session_id, "x-shared");
    assert!(uuid_of(&conn, "claude-code", "x-shared").is_none(), "the journal names a row without making one");
    // The migration journal: the finished move and the one in flight.
    let (done, live) = super::journals::read_migration(&conn).expect("migration journal");
    assert_eq!(done, vec![("claude-code/projects/-work/c-1.jsonl".to_owned(), 1790000000, "copied".to_owned())]);
    assert_eq!(live, vec![("codex/sessions/2026/10/02/rollout-x.jsonl".to_owned(), 1790000100)]);

    // Every JSON file byte-identical, and never touched.
    assert_eq!(snapshot(&dir), before, "the JSON stores are left exactly as they were");
    let after: Vec<_> = before.keys().map(|path| std::fs::metadata(path).and_then(|meta| meta.modified()).ok()).collect();
    assert_eq!(after, mtimes, "not one JSON file was rewritten");

    // A second launch imports nothing, even when a JSON file changed since.
    write(&dir, "history.json", r#"{"/work/shop": ["changed"]}"#);
    drop(conn);
    let conn = super::as_writer(|| super::open_at(&path)).expect("the second open");
    assert_eq!(count(&conn, "session"), 6);
    assert_eq!(super::history::read(&conn, "/work/shop").unwrap(), ["first", "second", "third"], "no second import");
    assert_eq!(count(&conn, "attachment"), 3);
    assert_eq!(count(&conn, "prompt_history"), 4);
    drop(conn);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A failure anywhere in the import leaves nothing behind: no row from it,
/// no marker, and the next run imports the whole set.
#[test]
fn a_failed_import_writes_nothing_and_runs_again() {
    let dir = temp("import-fails");
    real_shaped_stores(&dir);
    let before = snapshot(&dir);
    for step in [Step::Sessions, Step::Turns, Step::History, Step::Sent, Step::Journals] {
        let mut conn = super::open_memory();
        let failed = json_import::import(&mut conn, &dir, Some(step));
        assert!(failed.is_err(), "{step:?}: the injected error surfaces");
        for table in ["session", "turn", "attachment", "prompt_history", "import_job", "import_item", "migration_journal"] {
            assert_eq!(count(&conn, table), 0, "{step:?}: nothing from the import is in {table}");
        }
        assert!(super::schema::meta(&conn, super::schema::IMPORTED_KEY).unwrap().is_none(), "{step:?}: no marker");
        // The next launch imports everything.
        json_import::run_once(&mut conn, &dir).expect("the retry");
        assert_eq!(count(&conn, "session"), 6, "{step:?}: the retry imports it all");
        assert!(super::schema::meta(&conn, super::schema::IMPORTED_KEY).unwrap().is_some());
    }
    assert_eq!(snapshot(&dir), before, "a failed import touches no JSON either");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A store from before the split (`sessions.json`) is what is imported
/// when neither half exists; a file that does not parse is skipped and
/// reported, never renamed or rewritten; an entry that does not parse
/// drops only itself, and an unknown pane kind only that field.
#[test]
fn the_pre_split_store_imports_and_a_damaged_file_stays_as_it_was() {
    let dir = temp("legacy");
    write(
        &dir,
        "sessions.json",
        r#"{"a":{"name":"Old","lastSummary":"s","right":{"open":true,"kind":"quantum"}},"b":{"name":42},"c":{"pinned":true}}"#,
    );
    write(&dir, "provider-sessions.json", r#"{"broken": "#);
    let before = snapshot(&dir);
    let _ = crate::store::take_problems();
    let mut conn = super::open_memory();
    let report = json_import::import(&mut conn, &dir, None).expect("imports");
    assert_eq!(report.sessions, 2, "a and c; b does not parse and drops alone");
    let (facts, records) = rows::load_front(&conn, &mut FrontState::default()).expect("load");
    assert_eq!(facts["a"].name.as_deref(), Some("Old"));
    assert_eq!(facts["a"].last_summary.as_deref(), Some("s"));
    let right = facts["a"].right.as_ref().expect("the pane stays");
    assert!(right.open, "the rest of the pane state stays");
    assert_eq!(right.kind, crate::layout::RightKind::Files, "the unknown kind falls back");
    assert!(facts["c"].pinned);
    assert!(records.is_empty(), "the damaged records file imports nothing");
    let problems = crate::store::take_problems();
    assert!(problems.iter().any(|p| matches!(p, crate::store::StoreProblem::Unreadable { name, restored: false } if name == "provider-sessions.json")), "{problems:?}");
    assert!(problems.iter().any(|p| matches!(p, crate::store::StoreProblem::EntriesSkipped { count: 1, .. })), "{problems:?}");
    assert_eq!(snapshot(&dir), before, "neither file was set aside or rewritten");
    let _ = std::fs::remove_dir_all(&dir);
}

/// D-10: one id under two providers is two rows with two uuids; the
/// window's id stands for its record's provider.
#[test]
fn one_id_under_two_providers_is_two_rows() {
    let conn = super::open_memory();
    let claude = rows::ensure_session(&conn, "claude-code", "shared", "baaz").unwrap();
    let codex = rows::ensure_session(&conn, "codex", "shared", "baaz").unwrap();
    assert_ne!(claude, codex);
    assert_eq!(rows::ensure_session(&conn, "codex", "shared", "baaz").unwrap(), codex, "found again, not made twice");
    // An unattributed row is adopted by the first provider that names it.
    let bare = rows::ensure_session(&conn, "", "draft", "listed").unwrap();
    assert_eq!(rows::ensure_session(&conn, "codex", "draft", "baaz").unwrap(), bare, "same uuid, re-keyed");
    assert!(uuid_of(&conn, "", "draft").is_none());
}

/// The window's snapshots: only changed rows are written; a draft's facts
/// written before its record land on the row the record then claims; a row
/// whose facts and record both left goes, unless something hangs off it.
#[test]
fn the_windows_snapshots_write_one_row_per_session() {
    let mut conn = super::open_memory();
    let mut front = FrontState::default();
    let mut facts = Overrides::new();
    facts.insert("d-1".into(), SessionMeta { project: Some("p".into()), ..SessionMeta::default() });
    rows::save_meta(&mut conn, &mut front, facts.clone()).unwrap();
    assert!(uuid_of(&conn, "", "d-1").is_some(), "facts before a record: unattributed");
    let mut records = ProviderSessionStore::new();
    let mut store = ProviderSessionStore::new();
    crate::provider_sessions::upsert_open(&mut store, &mut facts, "codex", "d-1", Some("/w".into()), None, Some("Title".into()));
    records.extend(store);
    rows::save_records(&mut conn, &mut front, records.clone()).unwrap();
    rows::save_meta(&mut conn, &mut front, facts.clone()).unwrap();
    assert_eq!(count(&conn, "session"), 1, "one row, re-keyed");
    assert!(uuid_of(&conn, "codex", "d-1").is_some());
    let (loaded_facts, loaded_records) = rows::load_front(&conn, &mut FrontState::default()).unwrap();
    assert_eq!(loaded_facts, facts);
    assert_eq!(loaded_records, records);
    // A row with a sent turn stays when the window lets go of it.
    let uuid = uuid_of(&conn, "codex", "d-1").unwrap();
    conn.execute(
        "INSERT INTO attachment(id, session_id, ordinal, position, sent_text, kind, body) VALUES ('a', ?1, 0, 0, 't', 'file', '{}')",
        params![uuid],
    )
    .unwrap();
    rows::save_meta(&mut conn, &mut front, Overrides::new()).unwrap();
    rows::save_records(&mut conn, &mut front, ProviderSessionStore::new()).unwrap();
    assert_eq!(count(&conn, "session"), 1, "kept: a sent turn hangs off it");
    let (facts, records) = rows::load_front(&conn, &mut FrontState::default()).unwrap();
    assert!(facts.is_empty() && records.is_empty(), "but it leaves the list");
    // Forgetting takes the row and what hangs off it, and names the row it
    // took: the uuid its checkpoint refs are under (docs/82 6.3).
    assert_eq!(rows::forget(&mut conn, &mut front, "codex", "d-1").unwrap(), vec![uuid]);
    assert_eq!(count(&conn, "session"), 0);
    assert_eq!(count(&conn, "attachment"), 0, "cascaded");
}

/// D-7: a turn whose provider never says what it cost stores `NULL` with
/// `unknown`, never a `0.0` that reads as free; Claude Code's own `0.0` is
/// reported.
#[test]
fn an_unknown_cost_is_null_not_zero() {
    let conn = super::open_memory();
    let meta = aui_protocol::TurnMeta { model: "m".into(), ..aui_protocol::TurnMeta::default() };
    let codex = crate::usage::row_from_finished("s-c", "v", "t-1", 1, &meta).with_provider("codex");
    let claude = crate::usage::row_from_finished("s-a", "v", "t-1", 1, &meta).with_provider("claude-code");
    assert!(super::turns::record_ledger_row(&conn, &codex).unwrap());
    assert!(super::turns::record_ledger_row(&conn, &claude).unwrap());
    assert_eq!(super::turns::cost(&conn, "s-c", "t-1"), Some((None, "unknown".to_owned())));
    assert_eq!(super::turns::cost(&conn, "s-a", "t-1"), Some((Some(0.0), "reported".to_owned())));
}

/// docs/85 11.4: Baaz did not quit cleanly and baaz.db is damaged — the
/// next open restores the rolling backup on its own and says so once.
#[test]
fn a_damaged_database_after_an_unclean_exit_opens_from_its_backup() {
    let dir = temp("unclean");
    let path = super::path_in(&dir);
    super::as_writer(|| {
        let conn = super::open_at(&path).expect("first open");
        conn.execute_batch("INSERT INTO meta(key, value) VALUES ('probe', 'kept')").unwrap();
        // The rolling backup holds this state; the run then dies (no clean
        // exit: the open marker stays).
        drop(conn);
        let _ = std::fs::remove_file(super::integrity::rolling_backup(&path));
        let conn = super::open_at(&path).expect("second open takes a backup");
        drop(conn);
    });
    assert!(super::integrity::last_exit_unclean(&path));
    let mut bytes = std::fs::read(&path).unwrap();
    for byte in bytes.iter_mut().skip(100) {
        *byte = 0x5A;
    }
    std::fs::write(&path, bytes).unwrap();
    for side in ["db-wal", "db-shm"] {
        let _ = std::fs::remove_file(path.with_extension(side));
    }
    super::as_writer(|| {
        let conn = super::open_at(&path).expect("opens from the backup");
        assert_eq!(super::schema::meta(&conn, "probe").unwrap().as_deref(), Some("kept"));
    });
    assert!(
        crate::store::take_problems().iter().any(|p| matches!(p, crate::store::StoreProblem::DatabaseRestored { .. })),
        "one toast says so"
    );
}

/// docs/85 11.2: baaz.db is the one list source. The old list and index
/// cache files are imported at the first open and removed; Muse rows never
/// opened in Baaz get session rows of their own (with `created_by`); the
/// kept list and index read back exactly as saved, in the server's order.
#[test]
fn the_list_and_index_live_in_the_session_table() {
    let dir = temp("listing");
    let listing = crate::list_cache::ListCache {
        version: crate::list_cache::VERSION,
        source: "/muse/session-index.db".into(),
        full_at_ms: 1_700,
        sessions: vec![
            crate::list_cache::ListedRow { session_id: "m-2".into(), title: Some("Second".into()), updated_at: "2026-11-02T00:00:00Z".into(), ..Default::default() },
            crate::list_cache::ListedRow { session_id: "m-1".into(), title: Some("First".into()), turn_count: 3, updated_at: "2026-11-01T00:00:00Z".into(), ..Default::default() },
        ],
    };
    std::fs::write(dir.join("session-list-cache.json"), serde_json::to_vec(&listing).unwrap()).unwrap();
    let mut index = std::collections::HashMap::new();
    index.insert("m-1".to_owned(), crate::index::IndexEntry { title: "First".into(), created_by: Some("baaz".into()), ..Default::default() });
    std::fs::write(dir.join("index-cache.json"), serde_json::to_vec(&index).unwrap()).unwrap();
    let path = super::path_in(&dir);
    super::as_writer(|| {
        let mut conn = super::open_at(&path).expect("opens and imports");
        assert!(!dir.join("session-list-cache.json").exists() && !dir.join("index-cache.json").exists(), "the old files are gone");
        assert_eq!(super::listing::load_listing(&conn).unwrap(), Some(listing.clone()), "the list reads back in order");
        assert_eq!(super::listing::load_index(&conn).unwrap(), index, "the index facts read back");
        assert_eq!(super::listing::load_both(&conn).unwrap(), (Some(listing.clone()), index.clone()), "one query, both");
        assert!(uuid_of(&conn, "muse", "m-2").is_some(), "a never-opened Muse row has a session row");
        let created_by: Option<String> =
            conn.query_row("SELECT created_by FROM session WHERE provider_session_id = 'm-1'", [], |r| r.get(0)).unwrap();
        assert_eq!(created_by.as_deref(), Some("baaz"));
        // A newer list without m-2: it loses its listing, keeps its row.
        let mut newer = listing.clone();
        newer.sessions.remove(0);
        super::listing::save_listing(&mut conn, &newer).unwrap();
        assert_eq!(super::listing::load_listing(&conn).unwrap().map(|l| l.sessions.len()), Some(1));
        assert!(uuid_of(&conn, "muse", "m-2").is_some());
    });
}

/// docs/85 10.8 migration, from real-shaped data: a version-3 file that
/// still holds the legacy `usage_turns` copy reaches version 4 with the
/// ledger's time indexes, without the legacy table, and with every turn row
/// and session as they were; a batch of ledger rows still lands under one
/// session row each.
#[test]
fn version_four_indexes_the_ledger_and_drops_the_legacy_copy() {
    let dir = temp("v4");
    real_shaped_stores(&dir);
    let path = super::path_in(&dir);
    let meta = aui_protocol::TurnMeta { model: "m".into(), tokens_in: 10, ..aui_protocol::TurnMeta::default() };
    let (turns_before, sessions_before) = super::as_writer(|| {
        let mut conn = super::open_at(&path).expect("opens");
        let rows: Vec<_> = (0..5).map(|n| crate::usage::row_from_finished("s-v4", "v", &format!("t-{n}"), n, &meta).with_provider("codex")).collect();
        super::turns::record_ledger(&mut conn, &rows).expect("ledger");
        conn.execute_batch(
            "CREATE TABLE usage_turns(session_id TEXT, turn_id TEXT, PRIMARY KEY(session_id, turn_id));
             INSERT INTO usage_turns VALUES ('s-v4', 't-0');
             DROP INDEX turn_finished; DROP INDEX turn_provider_finished;
             PRAGMA user_version = 3;",
        )
        .expect("back to the version-3 shape");
        (count(&conn, "turn"), count(&conn, "session"))
    });
    super::close(path.clone());
    super::as_writer(|| {
        let conn = super::open_at(&path).expect("reopens");
        let version: i32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
        assert_eq!(version, super::schema::VERSION, "every step from 3 on ran");
        let index = |name: &str| conn.query_row("SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?1", [name], |_| Ok(())).is_ok();
        assert!(index("turn_finished") && index("turn_provider_finished"), "the ledger's time indexes");
        assert!(conn.query_row("SELECT 1 FROM sqlite_master WHERE name = 'usage_turns'", [], |_| Ok(())).is_err(), "the legacy copy, fully copied, is gone");
        assert_eq!((count(&conn, "turn"), count(&conn, "session")), (turns_before, sessions_before), "nothing else moved");
        let plan: String = conn
            .query_row("EXPLAIN QUERY PLAN SELECT * FROM turn WHERE provider = 'codex' AND finished_at > 2", [], |row| row.get(3))
            .unwrap();
        assert!(plan.contains("turn_provider_finished"), "{plan}");
    });
}

/// docs/85 14.1 migration, from real-shaped data: a version-5 file (the
/// first-launch import of real-shaped stores, then its ledger) reaches
/// version 6 with the task tables, every session and turn as they were.
#[test]
fn version_six_adds_the_task_record_and_moves_nothing() {
    let dir = temp("v6");
    real_shaped_stores(&dir);
    let path = super::path_in(&dir);
    let meta = aui_protocol::TurnMeta { model: "m".into(), tokens_in: 10, ..aui_protocol::TurnMeta::default() };
    let (turns_before, sessions_before) = super::as_writer(|| {
        let mut conn = super::open_at(&path).expect("opens");
        let rows: Vec<_> = (0..3).map(|n| crate::usage::row_from_finished("s-v6", "v", &format!("t-{n}"), n, &meta).with_provider("codex")).collect();
        super::turns::record_ledger(&mut conn, &rows).expect("ledger");
        conn.execute_batch("DROP TABLE task_outbox; DROP TABLE task; PRAGMA user_version = 5;").expect("back to the version-5 shape");
        (count(&conn, "turn"), count(&conn, "session"))
    });
    super::close(path.clone());
    super::as_writer(|| {
        let mut conn = super::open_at(&path).expect("reopens");
        let version: i32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
        assert_eq!(version, 6);
        assert_eq!((count(&conn, "turn"), count(&conn, "session")), (turns_before, sessions_before), "nothing else moved");
        let task = crate::tasks::sample("t-1", crate::tasks::Status::Queued);
        super::tasks::save_with(&mut conn, &task, Some(&crate::tasks::Effect::Start { generation: 0 })).expect("the new tables take a task");
        assert_eq!(super::tasks::load(&conn, "t-1").unwrap(), Some(task));
    });
    super::close(path);
}

/// docs/85 10.4: a compaction's ledger row is marked `role = 'compaction'`:
/// its cost is on the ledger, and the per-turn reader (`find_ledger`, which
/// asks for assistant turns) leaves it out.
#[test]
fn a_compaction_row_is_marked_and_kept_out_of_turn_lookups() {
    let conn = super::open_memory();
    let meta = aui_protocol::TurnMeta { model: "m".into(), cost_usd: 0.07, ..aui_protocol::TurnMeta::default() };
    let row = crate::usage::row_from_finished("s-k", "v", "compaction-1", 1, &meta).with_provider("claude-code").as_compaction();
    assert!(super::turns::record_ledger_row(&conn, &row).unwrap());
    let role: String = conn
        .query_row("SELECT role FROM turn WHERE turn_id = 'compaction-1'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(role, super::turns::ROLE_COMPACTION);
    assert_eq!(super::turns::cost(&conn, "s-k", "compaction-1"), Some((Some(0.07), "reported".to_owned())));
    assert!(super::turns::find_ledger(&conn, "s-k", "compaction-1").is_none());
}

/// D-3: a bubble text is keyed by its turn: two turns with the same echoed
/// text keep their own.
#[test]
fn display_texts_are_keyed_by_turn() {
    let conn = super::open_memory();
    super::turns::note_display(&conn, "codex", "s", "u-1", "same pack", "First").unwrap();
    super::turns::note_display(&conn, "codex", "s", "u-2", "same pack", "Second").unwrap();
    let displays = super::turns::displays(&conn, "codex", "s").unwrap();
    assert_eq!(displays.by_turn["u-1"], "First");
    assert_eq!(displays.by_turn["u-2"], "Second");
    assert!(displays.by_text.is_empty(), "a keyed row never answers by text");
}

/// D-8: forgetting a session removes its sent files and its hand-off
/// snapshot and offers the transcript in Baaz's own home — only that, and
/// never an imported session's (it lives in the person's home). The
/// decision only: nothing here touches the Trash.
#[test]
fn forgetting_offers_only_the_transcript_in_baazs_home() {
    let dir = temp("forget");
    write(&dir, "sent/c-1.json", "[]");
    write(&dir, "sent/c-1/a1.png", "png");
    write(&dir, "sent/c-2/b.png", "png");
    write(&dir, "handoff/c-1.json", "{}");
    write(&dir, "claude-home/projects/-work-shop/c-1.jsonl", "{}\n");
    write(&dir, "claude-home/projects/-work-shop/c-1/subagent.jsonl", "{}\n");
    write(&dir, "claude-home/projects/-work-shop/c-2.jsonl", "{}\n");
    write(&dir, "codex-home/sessions/2026/11/02/rollout-2026-11-02T09-00-00-x-1.jsonl", "{}\n");
    let plan = crate::sessions::forget_plan(&dir, "claude-code", "c-1", false);
    assert_eq!(plan.removed, vec![dir.join("sent/c-1"), dir.join("sent/c-1.json"), dir.join("handoff/c-1.json")]);
    assert_eq!(
        plan.transcript,
        vec![dir.join("claude-home/projects/-work-shop/c-1"), dir.join("claude-home/projects/-work-shop/c-1.jsonl")],
        "the transcript and its companion folder, nothing of c-2"
    );
    let codex = crate::sessions::forget_plan(&dir, "codex", "x-1", false);
    assert_eq!(codex.transcript, vec![dir.join("codex-home/sessions/2026/11/02/rollout-2026-11-02T09-00-00-x-1.jsonl")]);
    assert!(crate::sessions::forget_plan(&dir, "claude-code", "c-1", true).transcript.is_empty(), "an import's stays");
    assert!(crate::sessions::forget_plan(&dir, "muse", "c-1", false).transcript.is_empty(), "Muse keeps its own");
    assert!(crate::sessions::forget_plan(&dir, "claude-code", "../c-1", false).transcript.is_empty(), "no escaping the home");
    // A link out of the home is never followed.
    std::fs::remove_dir_all(dir.join("codex-home/sessions")).unwrap();
    std::os::unix::fs::symlink(dir.join("claude-home"), dir.join("codex-home/sessions")).unwrap();
    assert!(crate::sessions::forget_plan(&dir, "codex", "c-1", false).transcript.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Settings › Data › Prompt history (docs/82 8.8): Clear forgets every
/// workspace's prompts and nothing else.
#[test]
fn clearing_prompt_history_forgets_every_workspace_and_nothing_else() {
    let mut conn = super::open_memory();
    super::history::replace(&mut conn, "/work/a", &["one".to_owned(), "two".to_owned()]).expect("a");
    super::history::replace(&mut conn, "/work/b", &["three".to_owned()]).expect("b");
    let meta_before = count(&conn, "meta");
    assert_eq!(super::history::clear_all(&conn).expect("clear"), 3);
    assert!(super::history::read(&conn, "/work/a").unwrap().is_empty());
    assert!(super::history::read(&conn, "/work/b").unwrap().is_empty());
    assert_eq!(count(&conn, "meta"), meta_before, "only prompt history goes");
}
