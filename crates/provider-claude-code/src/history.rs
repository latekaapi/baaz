//! Stored history: `ReadSession` / `PageTranscript` from disk, not the stream.
//!
//! A resumed child does not replay its transcript on stdout (doc §3), so
//! history comes from the stored file:
//!
//! ```text
//! <claude-config-dir>/projects/<cwd-slug>/<session-id>.jsonl
//! ```
//!
//! where `<claude-config-dir>` is [`config_dir`] (the Baaz-owned
//! `claude-home` in production, `~/.claude` only when neither the env nor
//! an override says otherwise) and `<cwd-slug>` is the **resolved**
//! absolute cwd with `/` → `-` (`/private/tmp/ccprobe/work` →
//! `-private-tmp-ccprobe-work`). Doc §3 flags this as the least confident
//! part of the seam — `/tmp` resolves to `/private/tmp` and the slug
//! follows the resolved path — so this module resolves before slugging,
//! and when the directory is absent it degrades to an honest
//! empty/unavailable answer, never a guess at a different file.

use std::path::{Path, PathBuf};

/// The Claude config dir history resolves through: `$CLAUDE_CONFIG_DIR`
/// when set non-empty (what every claude child spawns with — the
/// Baaz-owned `claude-home`), else `home/.claude`. Every projects-path
/// lookup in this crate goes through here, so history follows the same dir
/// the children write to.
pub fn config_dir(home: &Path) -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".claude"))
}

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

/// The stored transcript path for one session under an explicit config
/// dir, or `None` when it cannot be named honestly: the cwd does not
/// resolve, or the slug directory is absent. `None` means
/// empty/unavailable — the caller must not try a different file.
pub fn transcript_path_for_config(
    cwd: &Path,
    session_id: &str,
    config_dir: &Path,
) -> Option<PathBuf> {
    let resolved = resolve_cwd(cwd).ok()?;
    let slug = slug_for_cwd(&resolved);
    let dir = config_dir.join("projects").join(slug);
    if !dir.is_dir() {
        return None;
    }
    Some(dir.join(format!("{session_id}.jsonl")))
}

/// The stored transcript path for one session, or `None` when it cannot be
/// named honestly: the cwd does not resolve, or the slug directory is
/// absent. `None` means empty/unavailable — the caller must not try a
/// different file.
///
/// `home` overrides `$HOME` (tests); `None` reads the environment. The
/// projects root resolves through [`config_dir`], so a set
/// `$CLAUDE_CONFIG_DIR` redirects this exactly the way it redirects the
/// children that wrote the file.
pub fn stored_transcript_path(
    cwd: &Path,
    session_id: &str,
    home: Option<&Path>,
) -> Option<PathBuf> {
    let home = match home {
        Some(home) => home.to_path_buf(),
        None => std::env::var_os("HOME").map(PathBuf::from)?,
    };
    transcript_path_for_config(cwd, session_id, &config_dir(&home))
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
pub(crate) mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cc-history-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tmpdir");
        dir
    }

    /// Serialize the tests that read or mutate `CLAUDE_CONFIG_DIR`: the
    /// runner executes tests on threads sharing one environment, and a
    /// set var redirects every config resolution in this binary.
    pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub(crate) fn lock_config_env() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) struct SavedConfigDir {
        previous: Option<std::ffi::OsString>,
    }

    impl SavedConfigDir {
        pub(crate) fn set(dir: &Path) -> Self {
            let previous = std::env::var_os("CLAUDE_CONFIG_DIR");
            std::env::set_var("CLAUDE_CONFIG_DIR", dir);
            Self { previous }
        }

        pub(crate) fn clear() -> Self {
            let previous = std::env::var_os("CLAUDE_CONFIG_DIR");
            std::env::remove_var("CLAUDE_CONFIG_DIR");
            Self { previous }
        }
    }

    impl Drop for SavedConfigDir {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(previous) => std::env::set_var("CLAUDE_CONFIG_DIR", previous),
                None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
            }
        }
    }

    #[test]
    fn slug_follows_the_resolved_path() {
        let _guard = lock_config_env();
        let _cleared = SavedConfigDir::clear();
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
        let _guard = lock_config_env();
        let _cleared = SavedConfigDir::clear();
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
        let _guard = lock_config_env();
        let _cleared = SavedConfigDir::clear();
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

    #[test]
    fn history_follows_claude_config_dir() {
        let _guard = lock_config_env();
        // Without the var, history reads `home/.claude`.
        let _cleared = SavedConfigDir::clear();
        let home = tmp("cfg-home");
        assert_eq!(config_dir(&home), home.join(".claude"));
        // An empty var is unset: the CLI treats only a non-empty value
        // as a redirect.
        std::env::set_var("CLAUDE_CONFIG_DIR", "");
        assert_eq!(config_dir(&home), home.join(".claude"));
        // A set var redirects the whole lookup: the file lives under it,
        // not under `home/.claude`.
        let work = tmp("cfg-work");
        let config = tmp("cfg-dir");
        let slug = slug_for_cwd(&resolve_cwd(&work).expect("work resolves"));
        let dir = config.join("projects").join(&slug);
        std::fs::create_dir_all(&dir).expect("slug dir under the config dir");
        std::fs::write(dir.join("sess-1.jsonl"), "{}\n").expect("plant");
        let _saved = SavedConfigDir::set(&config);
        assert_eq!(config_dir(&home), config);
        assert_eq!(
            stored_transcript_path(&work, "sess-1", Some(&home)),
            Some(dir.join("sess-1.jsonl")),
            "history follows CLAUDE_CONFIG_DIR, not ~/.claude"
        );
        assert_eq!(
            stored_transcript_path(&work, "sess-absent", Some(&home)),
            Some(dir.join("sess-absent.jsonl")),
            "an id names its would-be file under the redirected dir, as before"
        );
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&work);
        let _ = std::fs::remove_dir_all(&config);
    }

    #[test]
    fn an_explicit_config_dir_names_transcripts_directly() {
        let _guard = lock_config_env();
        let _cleared = SavedConfigDir::clear();
        let work = tmp("explicit-work");
        let config = tmp("explicit-cfg");
        let slug = slug_for_cwd(&resolve_cwd(&work).expect("work resolves"));
        let dir = config.join("projects").join(&slug);
        std::fs::create_dir_all(&dir).expect("slug dir");
        std::fs::write(dir.join("sess-9.jsonl"), "{}\n").expect("plant");
        assert_eq!(
            transcript_path_for_config(&work, "sess-9", &config),
            Some(dir.join("sess-9.jsonl"))
        );
        assert!(
            transcript_path_for_config(&work, "sess-9", &tmp("explicit-missing")).is_none(),
            "an absent slug dir names nothing"
        );
        let _ = std::fs::remove_dir_all(&work);
        let _ = std::fs::remove_dir_all(&config);
    }
}
