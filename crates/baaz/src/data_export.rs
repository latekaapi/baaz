//! Settings › Data › Export (docs/82 8.8): a folder copy of the person's own
//! Baaz data, written where they pick.
//!
//! What goes, and why each is safe to hand someone:
//!
//! * `baaz.db` — sessions, their records and turns, prompt history, usage —
//!   and `Browser/browser.db` — browsing history, bookmarks, site settings.
//!   Each is copied with SQLite's `VACUUM INTO` on a read-only connection,
//!   so a database the app is writing (WAL) exports as one consistent
//!   snapshot and the original is never touched.
//! * `layout.json` (settings and window layout), `projects.json`,
//!   `keymap.json` when there is one: plain copies.
//! * `README.txt`: what the folder holds and what it leaves out.
//!
//! What never goes: the provider homes (`claude-home`, `codex-home`, which
//! hold or link sign-ins), the browser engine's profile (`Browser/Chromium`:
//! cookies and site sign-ins), the search index (`search.db`, rebuilt from
//! the sessions), caches and logs. Baaz keeps no API key in its folder; a
//! key lives in the Keychain or with the provider's own CLI.
//!
//! The copy blocks: [`crate::settings::SettingsPage::export_data`] runs it on
//! the background executor. Pure over its two directories, so tests drive it
//! against temp dirs.

use std::path::{Path, PathBuf};

/// The SQLite databases an export snapshots, relative to the support dir.
pub(crate) const DATABASES: &[&str] = &["baaz.db", "Browser/browser.db"];

/// The JSON files an export copies when they exist, relative to the
/// support dir.
pub(crate) const FILES: &[&str] = &["layout.json", "projects.json", "keymap.json"];

/// The README an export carries.
const README: &str = "This folder is an export of your Baaz data.

baaz.db             Sessions, their records and turns, prompt history, usage.
Browser/browser.db  Browsing history, bookmarks and site settings.
layout.json         Settings and window layout.
projects.json       Your projects.
keymap.json         Your shortcut changes (only when you made some).

Left out on purpose: provider sign-ins and homes, the browser's cookies and
site sign-ins, the search index (Baaz rebuilds it), caches and logs.
Transcripts the providers keep in their own folders are not copied.
";

/// What one export wrote.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Exported {
    /// The new folder.
    pub folder: PathBuf,
    /// The files it holds, relative to the folder, README first.
    pub files: Vec<String>,
}

/// Write an export of `support` (Baaz's folder) into a new folder under
/// `parent`, named `Baaz export <stamp>` (` 2`, ` 3`… when taken). A
/// source that does not exist is skipped; a failed copy stops the export
/// and says which file.
pub(crate) fn export_into(support: &Path, parent: &Path, stamp: &str) -> Result<Exported, String> {
    let folder = free_folder(parent, &format!("Baaz export {stamp}"));
    std::fs::create_dir_all(&folder).map_err(|error| format!("could not create {}: {error}", folder.display()))?;
    std::fs::write(folder.join("README.txt"), README).map_err(|error| format!("README.txt: {error}"))?;
    let mut files = vec!["README.txt".to_owned()];
    for name in DATABASES {
        let source = support.join(name);
        if !source.is_file() {
            continue;
        }
        let target = folder.join(name);
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir).map_err(|error| format!("{name}: {error}"))?;
        }
        snapshot(&source, &target).map_err(|error| format!("{name}: {error}"))?;
        files.push((*name).to_owned());
    }
    for name in FILES {
        let source = support.join(name);
        if !source.is_file() {
            continue;
        }
        std::fs::copy(&source, folder.join(name)).map_err(|error| format!("{name}: {error}"))?;
        files.push((*name).to_owned());
    }
    Ok(Exported { folder, files })
}

/// `parent/name`, or `parent/name 2`, `name 3`… — the first that does not
/// exist yet, so an export never writes into an older one.
fn free_folder(parent: &Path, name: &str) -> PathBuf {
    let first = parent.join(name);
    if !first.exists() {
        return first;
    }
    (2..).map(|n| parent.join(format!("{name} {n}"))).find(|path| !path.exists()).expect("an unused name")
}

/// One consistent copy of the database at `source` into `target` (which
/// must not exist), through a read-only connection: `VACUUM INTO`.
fn snapshot(source: &Path, target: &Path) -> rusqlite::Result<()> {
    let connection = rusqlite::Connection::open_with_flags(
        source,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.execute("VACUUM INTO ?1", [target.to_string_lossy().as_ref()])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("baaz-export-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// The export holds the databases (as readable snapshots) and the
    /// JSON files, never the provider homes, the engine profile or the
    /// search index; a second export lands beside the first.
    #[test]
    fn an_export_copies_the_persons_data_and_never_their_sign_ins() {
        let support = temp("support");
        let parent = temp("parent");
        let db = rusqlite::Connection::open(support.join("baaz.db")).expect("db");
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE prompt_history(text TEXT); INSERT INTO prompt_history VALUES ('hello');")
            .expect("seed");
        std::fs::write(support.join("layout.json"), "{}").expect("layout");
        std::fs::write(support.join("projects.json"), "{}").expect("projects");
        std::fs::write(support.join("search.db"), "index").expect("index");
        for secret in ["claude-home/.claude.json", "codex-home/auth.json", "Browser/Chromium/Cookies"] {
            let path = support.join(secret);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
            std::fs::write(path, "secret").expect("secret");
        }
        let done = export_into(&support, &parent, "2026-11-21 0900").expect("export");
        assert_eq!(done.folder, parent.join("Baaz export 2026-11-21 0900"));
        assert_eq!(done.files, ["README.txt", "baaz.db", "layout.json", "projects.json"]);
        // The snapshot reads, with the row the open (WAL) database held.
        let copy = rusqlite::Connection::open(done.folder.join("baaz.db")).expect("copy opens");
        let text: String = copy.query_row("SELECT text FROM prompt_history", [], |row| row.get(0)).expect("row");
        assert_eq!(text, "hello");
        for never in ["search.db", "claude-home", "codex-home", "Browser/Chromium", "Browser"] {
            assert!(!done.folder.join(never).exists(), "{never} is never exported");
        }
        drop(db);
        let again = export_into(&support, &parent, "2026-11-21 0900").expect("again");
        assert_eq!(again.folder, parent.join("Baaz export 2026-11-21 0900 2"), "never into an older export");
        let _ = std::fs::remove_dir_all(&support);
        let _ = std::fs::remove_dir_all(&parent);
    }
}
