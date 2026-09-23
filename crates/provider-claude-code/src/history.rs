//! Stored history: `ReadSession` / `PageTranscript` from disk, not the stream.
//!
//! A resumed child does not replay its transcript on stdout (doc §3), so
//! history comes from the stored file:
//!
//! ```text
//! ~/.claude/projects/<cwd-slug>/<session-id>.jsonl
//! ```
//!
//! where `<cwd-slug>` is the **resolved** absolute cwd with `/` → `-`
//! (`/private/tmp/ccprobe/work` → `-private-tmp-ccprobe-work`). Doc §3
//! flags this as the least confident part of the seam — `/tmp` resolves to
//! `/private/tmp` and the slug follows the resolved path — so this module
//! resolves before slugging, and when the directory is absent it degrades
//! to an honest empty/unavailable answer, never a guess at a different file.

use std::path::{Path, PathBuf};

/// Resolve symlinks in `cwd` (`/tmp` → `/private/tmp` on macOS) before
/// slugging. Fails when the directory does not exist — the caller turns
/// that into empty/unavailable, never into a guess.
pub fn resolve_cwd(cwd: &Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(cwd)
}

/// Slug a resolved absolute cwd: `/` → `-`.
/// `/private/tmp/ccprobe/work` → `-private-tmp-ccprobe-work`.
pub fn slug_for_cwd(resolved: &Path) -> String {
    resolved.to_string_lossy().replace('/', "-")
}

/// The stored transcript path for one session, or `None` when it cannot be
/// named honestly: the cwd does not resolve, or the slug directory is
/// absent. `None` means empty/unavailable — the caller must not try a
/// different file.
///
/// `home` overrides `$HOME` (tests); `None` reads the environment.
pub fn stored_transcript_path(
    cwd: &Path,
    session_id: &str,
    home: Option<&Path>,
) -> Option<PathBuf> {
    let resolved = resolve_cwd(cwd).ok()?;
    let slug = slug_for_cwd(&resolved);
    let home = match home {
        Some(home) => home.to_path_buf(),
        None => std::env::var_os("HOME").map(PathBuf::from)?,
    };
    let dir = home.join(".claude").join("projects").join(slug);
    if !dir.is_dir() {
        return None;
    }
    Some(dir.join(format!("{session_id}.jsonl")))
}

/// Session ids with stored transcripts under one slug directory (file stems
/// of `*.jsonl`), newest first when metadata allows. Absent directory →
/// empty, honestly.
pub fn stored_session_ids(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut ids: Vec<(Option<std::time::SystemTime>, String)> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .map(|path| {
            let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_owned();
            (modified, id)
        })
        .collect();
    ids.sort_by_key(|item| std::cmp::Reverse(item.0));
    ids.into_iter().map(|(_, id)| id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cc-history-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tmpdir");
        dir
    }

    #[test]
    fn slug_follows_the_resolved_path() {
        let dir = tmp("slug");
        let resolved = resolve_cwd(&dir).expect("tmpdir resolves");
        let slug = slug_for_cwd(&resolved);
        assert!(slug.starts_with('-'), "absolute path slugs start with `-`: {slug}");
        assert!(!slug.contains('/'));
        assert_eq!(slug, resolved.to_string_lossy().replace('/', "-"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn absent_directory_degrades_to_none_never_a_guess() {
        let missing = PathBuf::from("/definitely/not/here/ccprobe-work");
        assert!(stored_transcript_path(&missing, "sess", Some(Path::new("/"))).is_none());
        // Present cwd but no slug dir under home: still None.
        let dir = tmp("noslug");
        let home = tmp("home-empty");
        assert!(stored_transcript_path(&dir, "sess", Some(&home)).is_none());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn stored_file_resolves_through_symlinked_tmp() {
        // The /tmp → /private/tmp counter-example from doc §3: the slug
        // must follow the resolved path, so plant the file under the
        // resolved slug and find it via the unresolved cwd.
        let home = tmp("home");
        let unresolved = PathBuf::from("/tmp");
        let Ok(resolved) = resolve_cwd(&unresolved) else { return };
        let slug = slug_for_cwd(&resolved);
        let dir = home.join(".claude").join("projects").join(&slug);
        std::fs::create_dir_all(&dir).expect("slug dir");
        std::fs::write(dir.join("sess-1.jsonl"), "{}\n").expect("plant");
        let found = stored_transcript_path(&unresolved, "sess-1", Some(&home));
        assert_eq!(found, Some(dir.join("sess-1.jsonl")));
        assert_eq!(stored_session_ids(&dir), ["sess-1"]);
        let _ = std::fs::remove_dir_all(&home);
    }
}
