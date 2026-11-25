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
pub const MEMORY_TOOL_NAMES: [&str; 4] = ["session_search", "session_read", "memory_save", "memory_edit"];

/// Answer one call. `root` is the calling session's project root and
/// `session` its id.
pub fn handle(tool: &str, params: &Value, root: &str, session: &str) -> Result<Value, String> {
    match tool {
        "session_search" => search(params, root),
        "session_read" => read(params, session),
        "memory_save" => save(params, root),
        "memory_edit" => edit(params, root),
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
    let run = |query: &str| crate::search::with_reader(|conn| crate::search::query_sessions(conn, query, 20)).unwrap_or_default();
    let facts = |id: &str| {
        let id = id.to_owned();
        baaz_core::db::read(baaz_core::db::path(), move |opened| baaz_core::session_tools::facts_from_db(&opened.conn, &id)).flatten()
    };
    let found = baaz_core::session_tools::search(&queries, (!all).then_some(&in_project as &dyn Fn(&str) -> bool), &run, &facts)?;
    Ok(json!({"results": found, "scope": if all { "all" } else { "project" }}))
}

fn read(params: &Value, current: &str) -> Result<Value, String> {
    let session = params.get("session").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(current).to_owned();
    let query = params.get("query").and_then(Value::as_str).map(str::to_owned);
    let lookup = session.clone();
    let facts = baaz_core::db::read(baaz_core::db::path(), move |opened| baaz_core::session_tools::facts_from_db(&opened.conn, &lookup))
        .flatten()
        .ok_or_else(|| format!("no session {session} in Baaz"))?;
    if facts.excluded {
        return Err("that is a side session; it has nothing to read".into());
    }
    let state = crate::store::support_dir();
    let turns = baaz_core::transcripts::with_store(&state, |conn| baaz_core::transcripts::read(conn, &facts.provider, &session))
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Baaz has not kept that session's turns yet (it keeps them from the first time it is opened in Baaz)".to_owned())?;
    Ok(json!({"session": session, "title": facts.title, "provider": facts.provider, "turns": baaz_core::session_tools::read(&turns, query.as_deref())}))
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
    Ok(json!({if change.delete { "removed" } else { "changed" }: name, "scope": scope.words()}))
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
        assert_eq!(removed["removed"], "release-day");
        assert!(!path.exists());
        assert!(handle("memory_save", &json!({"title": "x", "type": "gossip", "body": "y"}), &root, "s").is_err());
        assert!(handle("session_search", &json!({"queries": []}), &root, "s").is_err(), "phrasings are required");
        let _ = std::fs::remove_dir_all(&repo);
    }
}
