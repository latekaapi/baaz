//! `code_find`'s index (docs/85 12.6, D-Q7 "code map"): a project's file
//! contents and symbol definitions, searchable by the agent through one
//! bridge tool — never a repo map in the prompt (generated context files
//! cost 20–23% more and help nothing, arXiv 2602.11988).
//!
//! * **Where.** `<state dir>/code-index/<slug>.db`, one SQLite file per
//!   project, in Baaz's own folder — never inside the repo.
//! * **What.** Every file `.gitignore` lets through (the `ignore` walker,
//!   so nested ignores and `.git/info/exclude` count too), under
//!   [`MAX_FILE`] and not binary. Contents go into an FTS5 table that only
//!   finds files (`detail=none`: the smallest index); the lines come from
//!   reading the found files, so the index stays well under [`MAX_INDEX`].
//!   Symbols (definitions) are [`crate::code_symbols`]'.
//! * **Fresh.** A file is re-read when its size or modified time moved;
//!   [`refresh`] walks the project and [`refresh_paths`] takes the files a
//!   turn edited or the watcher saw.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

/// Files larger than this are not indexed.
pub const MAX_FILE: u64 = 1024 * 1024;

/// The most an index may take on disk (a test holds it on this repo).
pub const MAX_INDEX: u64 = 50 * 1024 * 1024;

/// Results a query returns at most.
pub const MAX_HITS: usize = 50;

/// The index file for the project at `root`.
pub fn db_path(state: &Path, root: &Path) -> PathBuf {
    let slug: String = root.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    state.join("code-index").join(format!("{slug}.db"))
}

/// Open (creating) a project's index.
pub fn open_at(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA auto_vacuum=INCREMENTAL;
         CREATE TABLE IF NOT EXISTS file(path TEXT PRIMARY KEY, size INTEGER NOT NULL, mtime INTEGER NOT NULL, rowid_text INTEGER NOT NULL) WITHOUT ROWID;
         CREATE VIRTUAL TABLE IF NOT EXISTS text USING fts5(body, tokenize = 'unicode61 remove_diacritics 2', detail = none);
         CREATE TABLE IF NOT EXISTS symbol(name TEXT NOT NULL, kind TEXT NOT NULL, path TEXT NOT NULL, line INTEGER NOT NULL);
         CREATE INDEX IF NOT EXISTS symbol_by_name ON symbol(name COLLATE NOCASE);
         CREATE INDEX IF NOT EXISTS symbol_by_path ON symbol(path);",
    )?;
    Ok(conn)
}

fn stamp(meta: &std::fs::Metadata) -> i64 {
    meta.modified().ok().and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |at| at.as_nanos() as i64)
}

/// Whether `bytes` look like text (no NUL in the first 8 KB, valid UTF-8).
fn is_text(bytes: &[u8]) -> bool {
    !bytes[..bytes.len().min(8192)].contains(&0) && std::str::from_utf8(bytes).is_ok()
}

/// What a refresh did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Refreshed {
    /// Files (re)read.
    pub read: usize,
    /// Files dropped (gone, ignored, grown too big).
    pub dropped: usize,
    /// Files unchanged.
    pub unchanged: usize,
}

/// The files the walker lets through under `root`, relative.
pub fn walk(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in ignore::WalkBuilder::new(root).hidden(false).git_ignore(true).git_exclude(true).parents(true).build().flatten() {
        let path = entry.path();
        if path.components().any(|c| c.as_os_str() == ".git") || !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if let Ok(rel) = path.strip_prefix(root) {
            files.push(rel.to_path_buf());
        }
    }
    files
}

/// Bring the index in line with the project: changed files re-read, gone
/// ones dropped. One transaction.
pub fn refresh(conn: &mut Connection, root: &Path) -> rusqlite::Result<Refreshed> {
    let files = walk(root);
    let keep: std::collections::HashSet<String> = files.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let tx = conn.transaction()?;
    let mut stats = Refreshed::default();
    let known: Vec<String> = {
        let mut statement = tx.prepare("SELECT path FROM file")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for path in known.iter().filter(|path| !keep.contains(*path)) {
        drop_file(&tx, path)?;
        stats.dropped += 1;
    }
    for rel in &files {
        match index_file(&tx, root, rel)? {
            Some(true) => stats.read += 1,
            Some(false) => stats.unchanged += 1,
            None => stats.dropped += 1,
        }
    }
    tx.commit()?;
    Ok(stats)
}

/// Re-read just these files (relative or absolute under `root`): the ones a
/// turn edited, or the watcher saw. A path the walker would skip (ignored,
/// gone) is dropped.
pub fn refresh_paths(conn: &mut Connection, root: &Path, paths: &[PathBuf]) -> rusqlite::Result<Refreshed> {
    let tx = conn.transaction()?;
    let mut stats = Refreshed::default();
    for path in paths {
        let rel = path.strip_prefix(root).unwrap_or(path).to_path_buf();
        let inside_git = rel.components().any(|c| c.as_os_str() == ".git");
        if inside_git || !root.join(&rel).is_file() || !walked(root, &rel) {
            if drop_file(&tx, &rel.to_string_lossy())? {
                stats.dropped += 1;
            }
            continue;
        }
        match index_file(&tx, root, &rel)? {
            Some(true) => stats.read += 1,
            Some(false) => stats.unchanged += 1,
            None => stats.dropped += 1,
        }
    }
    tx.commit()?;
    Ok(stats)
}

/// Whether the walker would take `rel` (its `.gitignore` rules).
fn walked(root: &Path, rel: &Path) -> bool {
    let mut builder = ignore::gitignore::GitignoreBuilder::new(root);
    let mut at = Some(root.join(rel));
    while let Some(dir) = at.as_ref().and_then(|p| p.parent()).map(Path::to_path_buf) {
        let file = dir.join(".gitignore");
        if file.is_file() {
            let _ = builder.add(file);
        }
        if dir == root {
            break;
        }
        at = Some(dir);
    }
    builder.build().map_or(true, |rules| !rules.matched_path_or_any_parents(rel, false).is_ignore())
}

/// Index one file; `Some(true)` read, `Some(false)` unchanged, `None`
/// dropped (too big, binary, unreadable).
fn index_file(conn: &Connection, root: &Path, rel: &Path) -> rusqlite::Result<Option<bool>> {
    let path = rel.to_string_lossy().into_owned();
    let Ok(meta) = std::fs::metadata(root.join(rel)) else {
        drop_file(conn, &path)?;
        return Ok(None);
    };
    let (size, mtime) = (meta.len() as i64, stamp(&meta));
    let known: Option<(i64, i64)> = conn
        .query_row("SELECT size, mtime FROM file WHERE path = ?1", params![path], |row| Ok((row.get(0)?, row.get(1)?)))
        .ok();
    if known == Some((size, mtime)) {
        return Ok(Some(false));
    }
    drop_file(conn, &path)?;
    if meta.len() > MAX_FILE {
        return Ok(None);
    }
    let Ok(bytes) = std::fs::read(root.join(rel)) else { return Ok(None) };
    if !is_text(&bytes) {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&bytes);
    conn.execute("INSERT INTO text(body) VALUES (?1)", params![text.as_ref()])?;
    let rowid = conn.last_insert_rowid();
    conn.execute("INSERT INTO file(path, size, mtime, rowid_text) VALUES (?1, ?2, ?3, ?4)", params![path, size, mtime, rowid])?;
    for symbol in crate::code_symbols::definitions(rel, &text) {
        conn.execute(
            "INSERT INTO symbol(name, kind, path, line) VALUES (?1, ?2, ?3, ?4)",
            params![symbol.name, symbol.kind, path, symbol.line as i64],
        )?;
    }
    Ok(Some(true))
}

/// Forget one file; true when it was known.
fn drop_file(conn: &Connection, path: &str) -> rusqlite::Result<bool> {
    let rowid: Option<i64> = conn.query_row("SELECT rowid_text FROM file WHERE path = ?1", params![path], |row| row.get(0)).ok();
    if let Some(rowid) = rowid {
        conn.execute("DELETE FROM text WHERE rowid = ?1", params![rowid])?;
        conn.execute("DELETE FROM file WHERE path = ?1", params![path])?;
    }
    conn.execute("DELETE FROM symbol WHERE path = ?1", params![path])?;
    Ok(rowid.is_some())
}

/// One place `code_find` names.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Hit {
    /// Relative to the project.
    pub path: String,
    /// 1-based.
    pub line: usize,
    /// The line, trimmed and capped.
    pub text: String,
    /// `definition` for a symbol's definition, else `text`.
    pub kind: String,
}

/// Lines containing `query` (case-blind): FTS5 finds the files holding
/// its words, then their lines are read.
pub fn find_text(conn: &Connection, root: &Path, query: &str) -> rusqlite::Result<Vec<Hit>> {
    let words: Vec<String> = query
        // The tokenizer splits on `_` too: each part is its own word (a
        // quoted run would be a phrase, which `detail=none` cannot answer).
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{}\"", w.replace('"', "")))
        .collect();
    if words.is_empty() {
        return Ok(Vec::new());
    }
    let mut statement = conn.prepare_cached(
        "SELECT file.path FROM text JOIN file ON file.rowid_text = text.rowid WHERE text MATCH ?1 ORDER BY bm25(text) LIMIT 200",
    )?;
    let paths: Vec<String> = statement.query_map(params![words.join(" AND ")], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
    let needle = query.to_lowercase();
    let mut hits = Vec::new();
    for path in paths {
        let Ok(text) = std::fs::read_to_string(root.join(&path)) else { continue };
        for (index, line) in text.lines().enumerate() {
            if line.to_lowercase().contains(&needle) {
                hits.push(Hit { path: path.clone(), line: index + 1, text: line.trim().chars().take(200).collect(), kind: "text".into() });
                if hits.len() == MAX_HITS {
                    return Ok(hits);
                }
            }
        }
    }
    Ok(hits)
}

/// A symbol: its definitions first (exact name, then prefix), then its
/// uses as text.
pub fn find_symbol(conn: &Connection, root: &Path, name: &str) -> rusqlite::Result<Vec<Hit>> {
    let mut statement = conn.prepare_cached(
        "SELECT path, line, kind, name FROM symbol WHERE name = ?1 COLLATE NOCASE
         UNION ALL SELECT path, line, kind, name FROM symbol WHERE name LIKE ?2 ESCAPE '\\' AND name <> ?1 COLLATE NOCASE
         LIMIT 20",
    )?;
    let prefix = format!("{}%", name.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
    let defs: Vec<(String, i64, String)> = statement
        .query_map(params![name, prefix], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut hits: Vec<Hit> = defs
        .into_iter()
        .map(|(path, line, kind)| {
            let text = std::fs::read_to_string(root.join(&path))
                .ok()
                .and_then(|t| t.lines().nth(line as usize - 1).map(|l| l.trim().chars().take(200).collect()))
                .unwrap_or_default();
            Hit { path, line: line as usize, text, kind: format!("definition ({kind})") }
        })
        .collect();
    for hit in find_text(conn, root, name)? {
        if !hits.iter().any(|known| known.path == hit.path && known.line == hit.line) && hits.len() < MAX_HITS {
            hits.push(hit);
        }
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("baaz-code-index-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("target/debug")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".gitignore"), "target/\n*.log\n").unwrap();
        std::fs::write(dir.join("src/lib.rs"), "pub fn parse_date_range(input: &str) -> Range {\n    todo!()\n}\n\nfn helper() { parse_date_range(\"x\"); }\n").unwrap();
        std::fs::write(dir.join("src/app.py"), "class PostcodeChecker:\n    def check(self, code):\n        return True\n").unwrap();
        std::fs::write(dir.join("target/debug/out.rs"), "fn parse_date_range() {}").unwrap();
        std::fs::write(dir.join("build.log"), "parse_date_range failed").unwrap();
        std::fs::write(dir.join("blob.bin"), [0u8, 159, 146, 150]).unwrap();
        dir
    }

    #[test]
    fn the_index_honours_gitignore_and_finds_text_and_definitions() {
        let root = repo("find");
        let mut conn = open_at(&root.join("index.db")).unwrap();
        let first = refresh(&mut conn, &root).unwrap();
        assert!(first.read >= 3, "{first:?}");
        let text = find_text(&conn, &root, "parse_date_range").unwrap();
        let places: Vec<(String, usize)> = text.iter().map(|h| (h.path.clone(), h.line)).collect();
        assert_eq!(places, [("src/lib.rs".to_owned(), 1), ("src/lib.rs".to_owned(), 5)], "ignored target/ and *.log stay out");
        let symbol = find_symbol(&conn, &root, "PostcodeChecker").unwrap();
        assert_eq!(symbol[0].path, "src/app.py");
        assert!(symbol[0].kind.starts_with("definition"), "{symbol:?}");
        let again = refresh(&mut conn, &root).unwrap();
        assert_eq!(again.read, 0, "nothing moved: nothing re-read ({again:?})");
        // An edit and a removal.
        std::fs::write(root.join("src/lib.rs"), "pub fn parse_window() {}\n").unwrap();
        std::fs::remove_file(root.join("src/app.py")).unwrap();
        refresh_paths(&mut conn, &root, &[root.join("src/lib.rs"), PathBuf::from("src/app.py")]).unwrap();
        assert!(find_text(&conn, &root, "parse_date_range").unwrap().is_empty());
        assert_eq!(find_symbol(&conn, &root, "parse_window").unwrap()[0].line, 1);
        assert!(find_symbol(&conn, &root, "PostcodeChecker").unwrap().is_empty(), "a removed file's symbols go");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The index of this repository's own crates stays far under 50 MB and
    /// a query answers under 50 ms.
    #[test]
    fn this_repository_indexes_small_and_answers_fast() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let db = std::env::temp_dir().join(format!("baaz-code-index-self-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        let mut conn = open_at(&db).unwrap();
        refresh(&mut conn, &root).unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        let size = std::fs::metadata(&db).unwrap().len();
        assert!(size < MAX_INDEX, "{size} bytes");
        let started = std::time::Instant::now();
        let hits = find_symbol(&conn, &root, "file_fingerprint").unwrap();
        let took = started.elapsed();
        assert!(hits.iter().any(|h| h.kind.starts_with("definition") && h.path.ends_with("transcripts.rs")), "{hits:?}");
        assert!(took < std::time::Duration::from_millis(if cfg!(debug_assertions) { 200 } else { 50 }), "{took:?}");
        eprintln!("crates/: index {} KB, file_fingerprint in {took:?}", size / 1024);
        let _ = std::fs::remove_file(&db);
    }
}
