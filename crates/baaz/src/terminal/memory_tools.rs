//! The service's answers to the four session and memory tools (docs/85
//! 12.3): `session_search`, `session_read`, `memory_save`, `memory_edit`.
//!
//! Each runs on a worker thread (search.db, baaz.db and the transcript
//! store are never read on the UI thread) and answers its reply channel
//! itself. The calling session is the bridge's, never the agent's word
//! (rule 20): its id and project root come from the service's registry.

use std::path::Path;

use serde_json::{json, Value};

/// The tools this module answers.
pub const MEMORY_TOOL_NAMES: [&str; 5] = ["session_search", "session_read", "memory_save", "memory_edit", "code_find"];

/// Answer one call. `root` is the calling session's project root and
/// `session` its id.
pub fn handle(tool: &str, params: &Value, root: &str, session: &str) -> Result<Value, String> {
    match tool {
        "session_search" => search(params, root),
        "session_read" => read(params, session),
        "memory_save" => save(params, root),
        "memory_edit" => edit(params, root),
        "code_find" => code_find(params, root),
        other => Err(format!("unknown tool: {other}")),
    }
}

fn search(params: &Value, root: &str) -> Result<Value, String> {
    let queries: Vec<String> = params
        .get("queries")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default();
    let all = params.get("scope").and_then(Value::as_str) == Some("all");
    let project = baaz_core::providers::main_repo_root(Path::new(root));
    let in_project = move |folder: &str| baaz_core::providers::main_repo_root(Path::new(folder)) == project;
    // Each phrasing's words, then the digests that mention them (12.4).
    let state = crate::store::support_dir();
    let run = |query: &str| {
        let mut hits = crate::search::with_reader(|conn| crate::search::query_sessions(conn, query, 20)).unwrap_or_default();
        let digests = baaz_core::transcripts::with_store(&state, |conn| baaz_core::digest::matching(conn, query, 10))
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
        for hit in digests {
            if !hits.iter().any(|known| known.session_id == hit.session_id) {
                hits.push(hit);
            }
        }
        hits
    };
    let facts = |id: &str| {
        let lookup = id.to_owned();
        let mut facts =
            baaz_core::db::read(baaz_core::db::path(), move |opened| baaz_core::session_tools::facts_from_db(&opened.conn, &lookup)).flatten()?;
        let digest = baaz_core::transcripts::with_store(&state, |conn| baaz_core::digest::load(conn, &facts.provider, id))
            .ok()
            .and_then(Result::ok)
            .flatten();
        if let Some((text, _)) = digest {
            facts.digest = text;
        }
        Some(facts)
    };
    let found = baaz_core::session_tools::search(&queries, (!all).then_some(&in_project as &dyn Fn(&str) -> bool), &run, &facts)?;
    // Each result carries a read-only preview the transcript row opens in
    // the right pane, written here, off the UI thread.
    let results: Vec<Value> = found
        .iter()
        .map(|result| {
            let mut value = serde_json::to_value(result).unwrap_or(Value::Null);
            if let Some(preview) = write_preview(&state, result) {
                value["preview"] = Value::String(preview.to_string_lossy().into_owned());
            }
            value
        })
        .collect();
    Ok(json!({"results": results, "scope": if all { "all" } else { "project" }}))
}

/// `<state>/session-previews/<id>.md` for a found session, from the
/// transcript store; `None` when the store has not kept it.
fn write_preview(state: &Path, found: &baaz_core::session_tools::Found) -> Option<std::path::PathBuf> {
    let turns = baaz_core::transcripts::with_store(state, |conn| baaz_core::transcripts::read(conn, &found.provider, &found.session))
        .ok()?
        .ok()??;
    let safe: String = found.session.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    let dir = state.join("session-previews");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{safe}.md"));
    std::fs::write(&path, baaz_core::session_tools::preview_markdown(found, &turns)).ok()?;
    Some(path)
}

fn read(params: &Value, current: &str) -> Result<Value, String> {
    // Agents spell the id's key their own way (seen live: `sessionId`).
    let session = ["session", "sessionId", "session_id", "id"]
        .iter()
        .find_map(|key| params.get(*key).and_then(Value::as_str).filter(|s| !s.is_empty()))
        .unwrap_or(current)
        .to_owned();
    let query = params.get("query").and_then(Value::as_str).map(str::to_owned);
    let lookup = session.clone();
    let facts = baaz_core::db::read(baaz_core::db::path(), move |opened| baaz_core::session_tools::facts_from_db(&opened.conn, &lookup))
        .flatten()
        .ok_or_else(|| format!("no session {session} in Baaz"))?;
    if facts.excluded {
        return Err("that is a side session; it has nothing to read".into());
    }
    let state = crate::store::support_dir();
    // A session Baaz never opened is read from the provider's own file
    // (docs/87 18.7), text only.
    let file = baaz_core::provider_search::stored_file(&facts.provider, &session);
    let turns = baaz_core::transcripts::with_store(&state, |conn| baaz_core::transcripts::read_or_fold(conn, &facts.provider, &session, file.as_deref()))
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Baaz cannot read that session's turns (it keeps none, and the provider left no file Baaz can read)".to_owned())?;
    Ok(json!({"session": session, "title": facts.title, "provider": facts.provider, "turns": baaz_core::session_tools::read(&turns, query.as_deref())}))
}

/// docs/85 12.6: one watched index per project (by main repo), made on the
/// first `code_find` and kept for the run.
fn code_find(params: &Value, root: &str) -> Result<Value, String> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    static WATCHED: Mutex<Option<HashMap<std::path::PathBuf, Arc<baaz_core::code_index::Watched>>>> = Mutex::new(None);
    let query = text(params, "query").ok_or("give a query")?;
    let kind = text(params, "kind");
    let project = baaz_core::providers::main_repo_root(Path::new(root));
    let watched = WATCHED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_or_insert_with(HashMap::new)
        .entry(project.clone())
        .or_insert_with(|| Arc::new(baaz_core::code_index::Watched::new(&crate::store::support_dir(), &project)))
        .clone();
    let hits = watched.find(query, kind)?;
    Ok(json!({"results": hits, "more": hits.len() == baaz_core::code_index::MAX_HITS}))
}

fn text<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

fn save(params: &Value, root: &str) -> Result<Value, String> {
    use baaz_core::memory::{self, Note, Scope};
    let scope = Scope::parse(text(params, "scope"))?;
    let note = Note {
        title: text(params, "title").ok_or("a note needs a title")?.to_owned(),
        description: text(params, "description").unwrap_or_default().to_owned(),
        kind: text(params, "type").unwrap_or("project").to_owned(),
        body: text(params, "body").ok_or("a note needs a body")?.to_owned(),
    };
    let dir = memory::dir_for(scope, Path::new(root));
    let path = memory::save(&dir, &note)?;
    Ok(json!({"saved": note.title, "scope": scope.words(), "path": path.to_string_lossy()}))
}

fn edit(params: &Value, root: &str) -> Result<Value, String> {
    use baaz_core::memory::{self, Change, Scope};
    let scope = Scope::parse(text(params, "scope"))?;
    let title = text(params, "title").ok_or("name the note by its title")?;
    let change = Change {
        body: text(params, "body").map(str::to_owned),
        description: text(params, "description").map(str::to_owned),
        delete: params.get("delete").and_then(Value::as_bool).unwrap_or(false),
    };
    let dir = memory::dir_for(scope, Path::new(root));
    let name = memory::edit(&dir, title, &change)?;
    let path = dir.join(format!("{}.md", memory::slug(&name)));
    Ok(json!({if change.delete { "removed" } else { "changed" }: name, "scope": scope.words(), "path": path.to_string_lossy()}))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A save, an edit and a forget through the service's handler, into the
    /// redirected memory folder of a throwaway repo (never the owner's).
    #[test]
    fn save_edit_and_forget_land_in_the_projects_shared_folder() {
        let repo = std::env::temp_dir().join(format!("baaz-memory-tools-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let root = repo.to_string_lossy().into_owned();
        let saved = handle("memory_save", &json!({"title": "Release day", "description": "When releases go out", "type": "project", "body": "Releases go out on Thursdays."}), &root, "s").unwrap();
        assert_eq!(saved["saved"], "Release day");
        assert_eq!(saved["scope"], "project memory");
        let path = std::path::PathBuf::from(saved["path"].as_str().unwrap());
        assert!(path.is_file());
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
        assert!(!path.starts_with(home.join(".claude")), "a test never writes the owner's memory: {}", path.display());
        handle("memory_edit", &json!({"title": "Release day", "body": "Releases go out on Fridays."}), &root, "s").unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("Fridays"));
        let removed = handle("memory_edit", &json!({"title": "Release day", "delete": true}), &root, "s").unwrap();
        assert_eq!(removed["removed"], "Release day");
        assert!(!path.exists());
        assert!(handle("memory_save", &json!({"title": "x", "type": "gossip", "body": "y"}), &root, "s").is_err());
        assert!(handle("session_search", &json!({"queries": []}), &root, "s").is_err(), "phrasings are required");
        // code_find over the same throwaway repo (12.6).
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/lib.rs"), "pub fn release_day() -> u8 { 4 }\n").unwrap();
        let found = handle("code_find", &json!({"query": "release_day", "kind": "symbol"}), &root, "s").unwrap();
        assert_eq!(found["results"][0]["path"], "src/lib.rs");
        assert!(found["results"][0]["kind"].as_str().unwrap().starts_with("definition"), "{found}");
        assert!(handle("code_find", &json!({"query": "x", "kind": "regex"}), &root, "s").is_err());
        let _ = std::fs::remove_dir_all(&repo);
    }
}
