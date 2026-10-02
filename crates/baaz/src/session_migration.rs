//! B4M: move Baaz's own provider sessions into Baaz's homes (owner-confirmed).
//!
//! B4 gave every Claude child `CLAUDE_CONFIG_DIR=<state>/claude-home` and
//! every Codex child `CODEX_HOME=<state>/codex-home`. Sessions Baaz opened
//! before that still have their transcripts/rollouts in the owner's
//! `~/.claude` / `~/.codex`, where the re-homed children can no longer
//! resume them. This module moves those files — never deletes anything
//! else, never writes the owner's `~/.codex` sqlite — keyed strictly on
//! Baaz's own registry ([`crate::provider_sessions`]), never on
//! heuristics, so unregistered probe/test leftovers stay where they are.
//!
//! Three surfaces share one executor:
//!
//! * the planner ([`plan_migration`]): pure over explicit dirs (temp dirs
//!   in tests — never the real `HOME`), listing only registered ids whose
//!   owner-side files still exist and whose Baaz-side targets do not
//!   (already-moved items are skipped: the plan is idempotent);
//! * the executor ([`execute_plan`]): `rename`, falling back to
//!   copy-plus-verify-plus-remove only across devices, with a journal file
//!   in the state dir ([`journal_path`]) so a crash resumes;
//! * the prompt (title/detail builders plus the dialog and Providers-row
//!   wiring on [`crate::app::Harness`]) and the lazy move on first resume
//!   ([`ensure_session_moved`]), all through the same [`move_one`].

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The registry wire id for Claude Code sessions.
pub const CLAUDE_PROVIDER: &str = "claude-code";
/// The registry wire id for Codex sessions.
pub const CODEX_PROVIDER: &str = "codex";
/// The journal file's name under the state dir.
pub const JOURNAL_FILE_NAME: &str = "session-migration-journal.json";
/// The one-time prompt's dismissal marker under the state dir.
pub const DISMISSED_FILE_NAME: &str = "session-migration.dismissed";
/// How many moved paths the prompt lists before folding the rest into a
/// count: the full list is one toggle away on the Providers row.
pub const MAX_LISTED_PATHS: usize = 12;

/// One file (or companion directory) to move: its absolute owner-side
/// source and its absolute Baaz-side target. The target keeps the source's
/// path relative to its home (`projects/<slug>/…`, `sessions/…/…`), so a
/// moved session resumes where the re-homed child looks for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedMove {
    /// [`CLAUDE_PROVIDER`] or [`CODEX_PROVIDER`].
    pub provider: String,
    /// The registry session/thread id.
    pub session_id: String,
    /// Absolute owner-side path.
    pub source: PathBuf,
    /// Absolute Baaz-home path.
    pub target: PathBuf,
    /// Whether the source is a directory (Claude's `<id>/` companion).
    pub is_dir: bool,
}

/// The owner's `$HOME` for migration IO: the real `HOME`, defaulting to
/// `.` when unset. Tests pass temp dirs explicitly and never call this.
///
/// `BAAZ_MIGRATION_OWNER_HOME` overrides it outright: the offline probe's
/// fixture owner home (see `scripts/uiprobe.py`'s `migrate-sessions`
/// entry), never a real home.
pub fn owner_home() -> PathBuf {
    if let Some(dir) = std::env::var_os("BAAZ_MIGRATION_OWNER_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// How many times [`plan_migration`] walked the owner homes in this
/// process: the render path must never move it (see
/// [`MigrationCache::plan_for_render`]), and the test below spies on
/// exactly that.
pub(crate) static PLAN_WALKS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Plan one registry record's moves: the owner-side files for its id that
/// still exist and whose Baaz-side targets do not. An id whose target
/// already exists was moved before — skipped, never re-copied. Unknown
/// providers plan nothing.
fn plan_one(
    owner_home: &Path,
    state_dir: &Path,
    provider: &str,
    session_id: &str,
) -> Vec<PlannedMove> {
    let mut moves = Vec::new();
    if provider == CLAUDE_PROVIDER {
        for entry in provider_claude_code::home::session_sources(owner_home, session_id) {
            let target = provider_claude_code::home::baaz_target(state_dir, &entry.rel);
            if target.exists() {
                continue;
            }
            moves.push(PlannedMove {
                provider: provider.to_owned(),
                session_id: session_id.to_owned(),
                source: entry.source,
                target,
                is_dir: entry.is_dir,
            });
        }
    } else if provider == CODEX_PROVIDER {
        for entry in provider_codex::home::session_sources(owner_home, session_id) {
            let target = provider_codex::home::baaz_target(state_dir, &entry.rel);
            if target.exists() {
                continue;
            }
            moves.push(PlannedMove {
                provider: provider.to_owned(),
                session_id: session_id.to_owned(),
                source: entry.source,
                target,
                is_dir: false,
            });
        }
    }
    moves
}

/// Plan the whole migration from the registry: every `claude-code` /
/// `codex` record's still-owner-side files. Records of other providers,
/// ids with no owner-side files, and already-moved items (target exists)
/// contribute nothing — and files no record names are never listed.
/// Sorted by (provider, session id, target) so the dry-run list is stable.
pub fn plan_migration(
    owner_home: &Path,
    state_dir: &Path,
    registry: &crate::provider_sessions::ProviderSessionStore,
) -> Vec<PlannedMove> {
    PLAN_WALKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut moves = Vec::new();
    for record in registry.values() {
        moves.extend(plan_one(owner_home, state_dir, &record.provider, &record.session_id));
    }
    moves.sort_by(|a, b| {
        (&a.provider, &a.session_id, &a.target).cmp(&(&b.provider, &b.session_id, &b.target))
    });
    moves
}

/// Plan a single session's moves (the lazy resume path): the same
/// [`plan_one`] the full plan uses, so resume moves exactly what the
/// prompt would have moved.
pub fn plan_single(
    owner_home: &Path,
    state_dir: &Path,
    provider: &str,
    session_id: &str,
) -> Vec<PlannedMove> {
    plan_one(owner_home, state_dir, provider, session_id)
}

/// How many planned moves belong to each provider: `(claude, codex)`.
pub fn counts(moves: &[PlannedMove]) -> (usize, usize) {
    let mut counts = (0usize, 0usize);
    for planned in moves {
        if planned.provider == CLAUDE_PROVIDER {
            counts.0 += 1;
        } else if planned.provider == CODEX_PROVIDER {
            counts.1 += 1;
        }
    }
    counts
}

// ---------------------------------------------------------------- journal

/// The journal file: the crash-resume record of finished moves.
pub fn journal_path(state_dir: &Path) -> PathBuf {
    state_dir.join(JOURNAL_FILE_NAME)
}

/// The journal key for one move: provider plus its Baaz-side relative
/// path, so two sessions never share a key.
pub fn move_key(planned: &PlannedMove) -> String {
    format!("{}/{}", planned.provider, planned.target.to_string_lossy())
}

/// Fully-done journal entries older than this are pruned on every run:
/// the filesystem reconcile below re-derives "done" from the targets, so
/// a pruned entry can never re-move anything.
const COMPLETED_TTL_SECS: u64 = 30 * 24 * 60 * 60;
/// In-progress records older than this belong to a crashed run, not a live
/// move: the executor holds one for seconds, so a day-old one is residue
/// and releases whatever staging it seemed to guard.
const IN_PROGRESS_TTL_SECS: u64 = 24 * 60 * 60;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// One finished move: when it landed and how. `at` is Unix seconds.
#[derive(Clone, Debug)]
struct CompletedEntry {
    at: u64,
    outcome: &'static str,
}

/// The journal, parsed best-effort like every store read: a missing or
/// unparseable journal is empty, and the target-exists skip keeps the
/// executor idempotent regardless.
///
/// Reads both shapes: the current `{"completed": {key: {at, outcome}},
/// "in_progress": {key: {at}}}` map and the first version's
/// `{"completed": [key, …]}` list (whose entries are stamped with now —
/// conservatively fresh, so they age out from this run, not from epoch).
fn read_journal(state_dir: &Path) -> (HashMap<String, CompletedEntry>, HashMap<String, u64>) {
    let mut done = HashMap::new();
    let mut live = HashMap::new();
    let text = std::fs::read_to_string(journal_path(state_dir)).unwrap_or_default();
    if text.is_empty() {
        return (done, live);
    }
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    if let Some(entries) = parsed.get("completed") {
        if let Some(map) = entries.as_object() {
            for (key, entry) in map {
                done.insert(
                    key.clone(),
                    CompletedEntry {
                        at: entry.get("at").and_then(|at| at.as_u64()).unwrap_or_else(now_secs),
                        outcome: if entry
                            .get("outcome")
                            .and_then(|outcome| outcome.as_str())
                            == Some("copied")
                        {
                            "copied"
                        } else {
                            "moved"
                        },
                    },
                );
            }
        } else if let Some(list) = entries.as_array() {
            // The first version's list shape: stamp with now (see above).
            let at = now_secs();
            for key in list.iter().filter_map(|key| key.as_str()) {
                done.insert(key.to_owned(), CompletedEntry { at, outcome: "moved" });
            }
        }
    }
    if let Some(map) = parsed.get("in_progress").and_then(|live| live.as_object()) {
        for (key, entry) in map {
            if let Some(at) = entry.get("at").and_then(|at| at.as_u64()) {
                live.insert(key.clone(), at);
            }
        }
    }
    (done, live)
}

/// Write the journal back, atomically. Best-effort: a journal that cannot
/// be written loses resume hints, never a session (the target-exists skip
/// still recognises every finished move).
fn write_journal(
    state_dir: &Path,
    done: &HashMap<String, CompletedEntry>,
    live: &HashMap<String, u64>,
) {
    let mut completed = serde_json::Map::new();
    let mut keys: Vec<&String> = done.keys().collect();
    keys.sort();
    for key in keys {
        let entry = &done[key];
        completed.insert(
            key.clone(),
            serde_json::json!({"at": entry.at, "outcome": entry.outcome}),
        );
    }
    let mut in_progress = serde_json::Map::new();
    let mut live_keys: Vec<&String> = live.keys().collect();
    live_keys.sort();
    for key in live_keys {
        in_progress.insert(key.clone(), serde_json::json!({"at": live[key]}));
    }
    let body = serde_json::json!({"completed": completed, "in_progress": in_progress});
    if let Ok(text) = serde_json::to_vec_pretty(&body) {
        let _ = crate::store::write_atomic(&journal_path(state_dir), &text);
    }
}

/// The finished keys the journal holds.
fn read_completed(state_dir: &Path) -> HashSet<String> {
    read_journal(state_dir).0.into_keys().collect()
}

/// Whether `key` has a live (fresh) in-progress record: a concurrent run
/// may be copying under it right now, so its staging is not residue.
fn has_live_record(state_dir: &Path, key: &str) -> bool {
    let (_, live) = read_journal(state_dir);
    live.get(key).is_some_and(|at| now_secs().saturating_sub(*at) < IN_PROGRESS_TTL_SECS)
}

/// Mark one move in flight, then finished: the crash-resume records.
/// Finished entries carry the landing time and how the file travelled, so
/// the journal records final state; the in-progress record exists only
/// while the bytes move, so staging with no live record is residue.
fn begin_move(state_dir: &Path, key: &str) {
    let (done, mut live) = read_journal(state_dir);
    live.insert(key.to_owned(), now_secs());
    write_journal(state_dir, &done, &live);
}

/// Record one finished move in the journal, atomically.
fn mark_completed(state_dir: &Path, key: &str, outcome: &'static str) {
    let (mut done, mut live) = read_journal(state_dir);
    live.remove(key);
    done.insert(key.to_owned(), CompletedEntry { at: now_secs(), outcome });
    write_journal(state_dir, &done, &live);
}

/// Drop fully-done entries older than 30 days and in-progress records
/// older than a day (crashed runs, never live moves). Pruning a finished
/// entry is safe: [`pending_moves`] re-derives "done" from the targets,
/// so a pruned entry retries nothing that already landed.
fn prune_journal(state_dir: &Path) {
    let (mut done, mut live) = read_journal(state_dir);
    if done.is_empty() && live.is_empty() {
        return;
    }
    let now = now_secs();
    done.retain(|_, entry| now.saturating_sub(entry.at) < COMPLETED_TTL_SECS);
    live.retain(|_, at| now.saturating_sub(*at) < IN_PROGRESS_TTL_SECS);
    write_journal(state_dir, &done, &live);
}

/// Drop the moves that already landed: the crash-resume filter, reconciled
/// against the filesystem rather than trusting the journal alone. A
/// journaled move whose target is missing while its source is still
/// present never landed — it is retried. The target-exists skip in
/// [`plan_migration`] already handles the finished-and-journaled case;
/// this covers a journal written for a move whose target check races it,
/// and a journal entry that outlives its target.
pub fn pending_moves(state_dir: &Path, moves: Vec<PlannedMove>) -> Vec<PlannedMove> {
    prune_journal(state_dir);
    let done = read_completed(state_dir);
    moves
        .into_iter()
        .filter(|planned| {
            if !done.contains(&move_key(planned)) {
                return true;
            }
            // Journaled but the target is gone while the source waits:
            // the move never landed — retry it.
            !planned.target.exists() && std::fs::symlink_metadata(&planned.source).is_ok()
        })
        .collect()
}

// ---------------------------------------------------------------- executor

/// What [`move_one`] did with a single source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveOutcome {
    /// `rename` carried it across.
    Moved,
    /// Across devices: copied, verified byte-for-byte, then removed.
    CopiedAcrossDevices,
    /// The target already holds it: an earlier run moved it.
    SkippedTargetExists,
    /// The source is gone (moved elsewhere, or never existed).
    SkippedSourceMissing,
}

/// Whether `error` is a cross-device rename refusal: only then does the
/// executor fall back to copy-plus-verify-plus-remove. Any other rename
/// failure propagates — retrying it as a copy would risk two live copies
/// of one session.
fn is_cross_device(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(18) || error.kind() == std::io::ErrorKind::CrossesDevices
}

/// The cross-device staging sibling for `target`: `.<name>.baaz-migrating`
/// beside the target, with no pid in the name — so any later run
/// recognises another run's crash residue and sweeps it (see
/// [`clean_stale_staging`]).
pub(crate) fn staging_path(target: &Path) -> PathBuf {
    let name = target.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    target.with_file_name(format!(".{name}.baaz-migrating"))
}

/// Sweep crash residue for this run's targets: a staging sibling with no
/// live journal-in-progress record belongs to no running copy, so it goes.
/// A live record means a concurrent run may be copying under it right now —
/// hands off. Best-effort: an unsweepable staging fails the run's own copy
/// below, never silently.
fn clean_stale_staging(state_dir: &Path, moves: &[PlannedMove]) {
    for planned in moves {
        let staging = staging_path(&planned.target);
        if std::fs::symlink_metadata(&staging).is_err() {
            continue;
        }
        if has_live_record(state_dir, &move_key(planned)) {
            continue;
        }
        eprintln!("baaz: migration: clearing stale staging {}", staging.display());
        let _ = staging.is_dir().then(|| std::fs::remove_dir_all(&staging)).unwrap_or_else(|| {
            std::fs::remove_file(&staging).or_else(|_| std::fs::remove_dir_all(&staging))
        });
    }
}

/// Copy one file or directory tree onto `target`, verifying before the
/// caller removes the source: every byte must read back identical, or the
/// source stays and the error propagates. Directories copy through a
/// sibling staging entry (never a partial target), then commit by rename.
fn copy_and_verify(source: &Path, target: &Path, is_dir: bool) -> std::io::Result<()> {
    // A staging entry from a crashed copy is residue, not data: clear it.
    let staging = staging_path(target);
    if let Some(parent) = staging.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::symlink_metadata(&staging).is_ok() {
        if staging.is_dir() {
            std::fs::remove_dir_all(&staging)?;
        } else {
            std::fs::remove_file(&staging).or_else(|_| std::fs::remove_dir_all(&staging))?;
        }
    }
    if is_dir {
        copy_dir_all(source, &staging)?;
        verify_tree(source, &staging)?;
        std::fs::rename(&staging, target)?;
    } else {
        std::fs::copy(source, &staging)?;
        verify_file(source, &staging)?;
        std::fs::rename(&staging, target)?;
    }
    Ok(())
}

/// Refuse a cross-device copy of anything the copier cannot carry: a tree
/// holding a symlink or a special entry would arrive without it and then
/// lose the original to the source remove — so the move never starts.
/// Same-device renames carry everything intact and are unaffected.
fn check_tree_copyable(source: &Path, is_dir: bool) -> std::io::Result<()> {
    if !is_dir {
        let kind = std::fs::symlink_metadata(source)?.file_type();
        if kind.is_symlink() || (!kind.is_file() && !kind.is_dir()) {
            return Err(refused(source));
        }
        return Ok(());
    }
    let mut stack = vec![source.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() || (!kind.is_file() && !kind.is_dir()) {
                return Err(refused(&entry.path()));
            }
            if kind.is_dir() {
                stack.push(entry.path());
            }
        }
    }
    Ok(())
}

/// The refusal error: what it holds, and that the owner moves it by hand.
/// Surfaced through the report's errors (user-visible) and the log.
fn refused(path: &Path) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!(
            "refused: {} is a symlink or special file, which the cross-device copy cannot carry; move it by hand",
            path.display()
        ),
    )
}

/// One entry of a pre-remove source snapshot: what the cross-device path
/// compares after copying, immediately before removing anything.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SnapEntry {
    rel: PathBuf,
    is_dir: bool,
    size: u64,
    mtime: Option<std::time::SystemTime>,
}

fn snap_one(path: &Path, rel: PathBuf) -> std::io::Result<SnapEntry> {
    let meta = std::fs::symlink_metadata(path)?;
    Ok(SnapEntry {
        rel,
        is_dir: meta.file_type().is_dir(),
        size: meta.len(),
        mtime: meta.modified().ok(),
    })
}

/// Sizes plus mtimes of the whole source tree (no follows: symlinks were
/// already refused above, so any entry that is not a file or dir here is a
/// concurrent change and fails the comparison below).
fn snapshot_source(source: &Path, is_dir: bool) -> std::io::Result<Vec<SnapEntry>> {
    if !is_dir {
        return Ok(vec![snap_one(source, PathBuf::new())?]);
    }
    let mut out = vec![snap_one(source, PathBuf::new())?];
    let mut stack = vec![source.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let rel = path.strip_prefix(source).unwrap_or(&path).to_path_buf();
            out.push(snap_one(&path, rel)?);
            if entry.file_type()?.is_dir() {
                stack.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
pub(crate) static FORCE_CROSS_DEVICE_FOR_TESTS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
pub(crate) static DIRTY_BETWEEN_COPY_AND_REMOVE_FOR_TESTS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Whether this move must take the cross-device copy path even when a
/// rename would succeed: the tests' injection hook for that branch.
/// Production never sets it.
fn force_copy_path() -> bool {
    #[cfg(test)]
    {
        FORCE_CROSS_DEVICE_FOR_TESTS.load(std::sync::atomic::Ordering::Relaxed)
    }
    #[cfg(not(test))]
    {
        false
    }
}

/// The cross-device leg of [`move_one`]: refuse uncopyable trees, copy and
/// verify, then re-verify the source is unchanged (sizes plus mtimes)
/// immediately before removing it. A source that changed mid-copy keeps
/// both copies and reports: deleting it would lose the newer bytes.
fn cross_device_move(source: &Path, target: &Path, is_dir: bool) -> std::io::Result<MoveOutcome> {
    check_tree_copyable(source, is_dir)?;
    let before = snapshot_source(source, is_dir)?;
    copy_and_verify(source, target, is_dir)?;
    #[cfg(test)]
    if DIRTY_BETWEEN_COPY_AND_REMOVE_FOR_TESTS.load(std::sync::atomic::Ordering::Relaxed) {
        // The test hook's concurrent writer: one more byte lands between
        // the copy and the remove, the way a racing write would.
        if is_dir {
            std::fs::write(source.join("race.jsonl"), "{}\n")?;
        } else {
            let mut body = std::fs::read(source)?;
            body.push(b'\n');
            std::fs::write(source, body)?;
        }
    }
    let after = snapshot_source(source, is_dir)?;
    if before != after {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "source changed during the copy of {}; kept both copies — move the newer bytes by hand",
                source.display()
            ),
        ));
    }
    if is_dir {
        std::fs::remove_dir_all(source)?;
    } else {
        std::fs::remove_file(source)?;
    }
    Ok(MoveOutcome::CopiedAcrossDevices)
}

/// Recursively copy a directory tree (symlinks are not followed: an entry
/// that is neither a dir nor a file is skipped, never chased).
fn copy_dir_all(source: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let dst = target.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_dir_all(&entry.path(), &dst)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), dst)?;
        }
    }
    Ok(())
}

/// Every file under `source` reads back identical under `target`: same
/// relative names, same bytes. Anything else is a failed copy.
fn verify_tree(source: &Path, target: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let dst = target.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            verify_tree(&entry.path(), &dst)?;
        } else if kind.is_file() {
            verify_file(&entry.path(), &dst)?;
        }
    }
    Ok(())
}

/// Two files hold the same bytes, or the copy failed.
fn verify_file(source: &Path, target: &Path) -> std::io::Result<()> {
    let (a, b) = (std::fs::read(source)?, std::fs::read(target)?);
    if a == b {
        Ok(())
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "copied bytes differ"))
    }
}

/// Move one source onto its target: `rename`, falling back to the
/// cross-device leg ([`cross_device_move`]: refuse uncopyable trees, copy,
/// verify, re-verify the source unchanged, then remove) only across
/// devices. Nothing else is ever removed — a refused, failed, unverified
/// or changed-mid-copy source stays in place and reports the error. The
/// target's parent directories are created; an existing target (or a
/// missing source) is a skip, never an overwrite.
pub fn move_one(source: &Path, target: &Path, is_dir: bool) -> std::io::Result<MoveOutcome> {
    if target.exists() || std::fs::symlink_metadata(target).is_ok() {
        return Ok(MoveOutcome::SkippedTargetExists);
    }
    if std::fs::symlink_metadata(source).is_err() {
        return Ok(MoveOutcome::SkippedSourceMissing);
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if force_copy_path() {
        return cross_device_move(source, target, is_dir);
    }
    match std::fs::rename(source, target) {
        Ok(()) => Ok(MoveOutcome::Moved),
        Err(error) if is_cross_device(&error) => cross_device_move(source, target, is_dir),
        Err(error) => Err(error),
    }
}

/// What [`execute_plan`] moved, skipped and failed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MigrationReport {
    /// Moves `rename` carried across.
    pub moved: usize,
    /// Moves the cross-device copy path carried across.
    pub copied: usize,
    /// Sources already gone or targets already holding the file.
    pub skipped: usize,
    /// `"<target>: <error>"` per failed move. Failed moves journal
    /// nothing, so the next run retries them.
    pub errors: Vec<String>,
}

/// Run the plan through [`move_one`], journaling every finished move so a
/// crash resumes where it stopped. Each run first sweeps crash-residue
/// staging with no live journal record. Skips (gone sources, held targets)
/// and failures (refusals, changed-mid-copy, IO) never journal as done:
/// skips need no resume, failures must retry — and every failure lands in
/// the report with its reason (user-visible) and on stderr (logged), with
/// both copies left in place.
pub fn execute_plan(state_dir: &Path, moves: &[PlannedMove]) -> MigrationReport {
    // One executor at a time: the prompt's confirm and a lazy move before a
    // resume both run in the background, and the journal is a
    // read-modify-write file. Serialising them keeps journal entries and the
    // staging sweep from racing each other.
    static EXECUTOR: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _executing = EXECUTOR.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    clean_stale_staging(state_dir, moves);
    let mut report = MigrationReport::default();
    for planned in pending_moves(state_dir, moves.to_vec()) {
        let key = move_key(&planned);
        begin_move(state_dir, &key);
        match move_one(&planned.source, &planned.target, planned.is_dir) {
            Ok(MoveOutcome::Moved) => {
                report.moved += 1;
                mark_completed(state_dir, &key, "moved");
            }
            Ok(MoveOutcome::CopiedAcrossDevices) => {
                report.copied += 1;
                mark_completed(state_dir, &key, "copied");
            }
            Ok(MoveOutcome::SkippedTargetExists | MoveOutcome::SkippedSourceMissing) => {
                report.skipped += 1;
                // No resume hint for a skip — but drop any in-progress
                // record this run wrote above, so it never guards residue.
                let (done, mut live) = read_journal(state_dir);
                if live.remove(&key).is_some() {
                    write_journal(state_dir, &done, &live);
                }
            }
            Err(error) => {
                eprintln!("baaz: migration: {}: {error}", planned.target.display());
                report.errors.push(format!("{}: {error}", planned.target.display()));
                let (done, mut live) = read_journal(state_dir);
                if live.remove(&key).is_some() {
                    write_journal(state_dir, &done, &live);
                }
            }
        }
    }
    report
}

// ---------------------------------------------------------------- prompt

/// `"Move 2 Claude Code and 1 Codex sessions into Baaz"`: the one-time
/// prompt's title (and the Providers row's) from the provider counts.
pub fn prompt_title(n_claude: usize, n_codex: usize) -> String {
    let mut parts = Vec::new();
    if n_claude > 0 {
        parts.push(format!("{n_claude} Claude Code"));
    }
    if n_codex > 0 {
        parts.push(format!("{n_codex} Codex"));
    }
    format!("Move {} sessions into Baaz", parts.join(" and "))
}

/// The prompt's body: what moves, where it lands, the dry-run paths
/// (first [`MAX_LISTED_PATHS`], then a count), and the two honest notes —
/// Codex for Mac keeps stale index rows Baaz never touches (the owner
/// archives them there), and nothing is ever deleted.
pub fn prompt_detail(moves: &[PlannedMove]) -> String {
    let (listed, rest) = prompt_paths_preview(moves);
    let mut detail = String::from(
        "Baaz's own sessions still live in the owner's Claude and Codex homes, where \
         Baaz's re-homed children can no longer resume them. Moving keeps the same \
         relative paths under Baaz's homes, so resume keeps working. Nothing is deleted.",
    );
    if !listed.is_empty() {
        detail.push_str("\n\nMoves:\n");
        for path in &listed {
            detail.push_str("- ");
            detail.push_str(path);
            detail.push('\n');
        }
        if rest > 0 {
            detail.push_str(&format!("…and {rest} more (see Settings → Providers).\n"));
        }
    }
    detail.push_str(
        "\nNote: Codex for Mac may keep listing moved threads until they are archived \
         there — Baaz never writes the owner's Codex index, so archive them in that app.",
    );
    detail
}

/// The dry-run path preview: the first [`MAX_LISTED_PATHS`] target paths
/// (relative to the state dir, so the list reads the same on any Mac)
/// plus how many more follow.
pub fn prompt_paths_preview(moves: &[PlannedMove]) -> (Vec<String>, usize) {
    let mut listed: Vec<String> = moves
        .iter()
        .take(MAX_LISTED_PATHS)
        .map(|planned| preview_path(&planned.target))
        .collect();
    listed.sort();
    let rest = moves.len().saturating_sub(listed.len());
    (listed, rest)
}

/// Every planned target for the Providers row's expandable list, sorted
/// and capped: the dry-run paths in full, not just the prompt's preview.
pub fn row_paths(moves: &[PlannedMove]) -> Vec<String> {
    let mut paths: Vec<String> = moves.iter().map(|planned| preview_path(&planned.target)).collect();
    paths.sort();
    paths.truncate(200);
    paths
}

/// Render a target path for the dry-run list: relative to its Baaz home
/// (`claude-home/…`, `codex-home/…`) when it lives under the state dir,
/// absolute otherwise.
fn preview_path(target: &Path) -> String {
    const HOMES: [&str; 2] = ["claude-home", "codex-home"];
    for home in HOMES {
        if let Some(rest) = target
            .to_string_lossy()
            .find(&format!("/{home}/"))
            .map(|at| target.to_string_lossy()[(at + 1)..].to_owned())
        {
            return rest;
        }
    }
    target.to_string_lossy().into_owned()
}

// -------------------------------------------------------------- dismissal

/// The dismissal marker: the one-time prompt's "Not now".
pub fn dismissal_path(state_dir: &Path) -> PathBuf {
    state_dir.join(DISMISSED_FILE_NAME)
}

/// Whether the owner dismissed the one-time prompt.
pub fn is_dismissed(state_dir: &Path) -> bool {
    dismissal_path(state_dir).exists()
}

/// Record the dismissal: the auto-prompt never shows again (the Providers
/// row stays while moves remain, so the migration is always one click
/// away). Best-effort: an unwritable marker only repeats the prompt.
pub fn dismiss(state_dir: &Path) {
    if let Some(parent) = dismissal_path(state_dir).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(dismissal_path(state_dir), "not-now\n");
}

// -------------------------------------------------------------------- lazy

/// Move one session's owner-side files into the Baaz homes, if any remain:
/// the lazy path `reopen_provider` runs before a resume, through the same
/// [`move_one`] executor the prompt uses, so a session that was never
/// moved resumes instead of failing under the new home. Returns how many
/// files moved. Unknown providers move nothing.
pub fn ensure_session_moved(
    owner_home: &Path,
    state_dir: &Path,
    provider: &str,
    session_id: &str,
) -> usize {
    let moves = pending_moves(state_dir, plan_single(owner_home, state_dir, provider, session_id));
    let report = execute_plan(state_dir, &moves);
    report.moved + report.copied
}

// ------------------------------------------------------------------- UI

use aui::overlay::DialogKind;

use crate::app::Harness as BaazHarness;
use crate::overlays::{Dialog, DialogAction};

/// The migration plan, computed once off the UI thread and read by every
/// render: the walk (`plan_migration`, a recursive owner-home scan per
/// record) never runs on the render path — renders only
/// [`MigrationCache::plan_for_render`], which never walks (see the
/// `PLAN_WALKS` spy below).
#[derive(Clone, Debug)]
pub(crate) struct MigrationCache {
    plan: Vec<PlannedMove>,
    computed_at: std::time::Instant,
}

impl Default for MigrationCache {
    fn default() -> Self {
        Self { plan: Vec::new(), computed_at: std::time::Instant::now() }
    }
}

impl MigrationCache {
    fn fresh(plan: Vec<PlannedMove>) -> Self {
        Self { plan, computed_at: std::time::Instant::now() }
    }

    /// The cached plan for renders: a pure slice read, no home walk.
    pub(crate) fn plan_for_render(&self) -> &[PlannedMove] {
        &self.plan
    }

    fn age_secs(&self) -> u64 {
        self.computed_at.elapsed().as_secs()
    }
}

/// A cached plan counts as fresh for ten minutes: renders read it as-is,
/// and only a stale or missing cache kicks a background recompute.
const CACHE_STALE_SECS: u64 = 600;

impl BaazHarness {
    /// This window's cached migration plan for renders: the cache, or
    /// nothing while the background compute is still running. Never
    /// walks the owner homes — that happens only in
    /// [`Self::refresh_migration_cache`], off the UI thread.
    pub(crate) fn migration_cached_plan(&self) -> Vec<PlannedMove> {
        self.migration_cache.as_ref().map(|cache| cache.plan.clone()).unwrap_or_default()
    }

    /// Recompute the plan off the UI thread and cache it with a timestamp:
    /// the registry and both dirs travel into a background task, and the
    /// cache (plus a re-render) lands when it completes. Re-entrant-safe:
    /// a second call while one is in flight is a no-op. Renders keep
    /// reading the old cache meanwhile — never the walker.
    pub(crate) fn refresh_migration_cache(&mut self, cx: &mut gpui::Context<Self>) {
        if self.migration_refresh_in_flight {
            return;
        }
        self.migration_refresh_in_flight = true;
        let owner = owner_home();
        let state = crate::store::support_dir();
        let registry = self.provider_sessions.clone();
        cx.spawn(async move |this, cx| {
            let plan = cx
                .background_executor()
                .spawn(async move { plan_migration(&owner, &state, &registry) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.migration_refresh_in_flight = false;
                this.migration_cache = Some(MigrationCache::fresh(plan));
                // A startup offer deferred on this compute re-checks now
                // that the plan is here; a no-op when nothing waits.
                this.maybe_offer_session_migration(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Drop the cached plan and recompute it off the UI thread: what a
    /// finished confirm or lazy move calls, so the Providers row follows
    /// the move without ever walking on the render path.
    pub(crate) fn invalidate_migration_cache(&mut self, cx: &mut gpui::Context<Self>) {
        self.migration_cache = None;
        self.refresh_migration_cache(cx);
    }

    /// The one-time prompt's dialog for `plan`: the "Move N …" title, the
    /// dry-run body, Move primary and Not-now secondary. Nothing moves
    /// without the click — building the dialog moves nothing.
    pub(crate) fn migration_dialog(plan: &[PlannedMove]) -> Dialog {
        let (n_claude, n_codex) = counts(plan);
        Dialog {
            title: prompt_title(n_claude, n_codex),
            detail: prompt_detail(plan),
            kind: DialogKind::Info,
            primary: "Move",
            action: DialogAction::MigrateSessions,
            archive_target: None,
        }
    }

    /// Offer the one-time prompt when moves remain and the owner has not
    /// dismissed it: deferred from startup, never over another dialog, a
    /// connect screen, or a deterministic capture. Reads only the cache —
    /// a missing or stale cache kicks a background recompute and offers
    /// when it lands, so the walk never runs on the UI thread.
    pub(crate) fn maybe_offer_session_migration(&mut self, cx: &mut gpui::Context<Self>) {
        if self.show_connect || crate::clock::deterministic() {
            return;
        }
        let state = crate::store::support_dir();
        if is_dismissed(&state) {
            return;
        }
        if self.overlays.read(cx).dialog.is_some() {
            return;
        }
        let fresh = self.migration_cache.as_ref().is_some_and(|cache| cache.age_secs() < CACHE_STALE_SECS);
        if !fresh {
            if !self.migration_refresh_in_flight {
                self.refresh_migration_cache(cx);
            }
            return;
        }
        let plan = self.migration_cached_plan();
        if plan.is_empty() {
            return;
        }
        self.set_dialog(cx, Self::migration_dialog(&plan));
    }

    /// Run the prompt's Move: the cached plan through the journaling
    /// executor on the background executor, then (back on the UI thread)
    /// a toast with the honest counts (moved, already there, failed) and
    /// a cache refresh so the Providers row follows. The prompt never
    /// shows again afterwards. The click returns at once — nothing walks
    /// or copies on the UI thread.
    pub(crate) fn confirm_session_migration(&mut self, cx: &mut gpui::Context<Self>) {
        let state = crate::store::support_dir();
        let plan = self.migration_cached_plan();
        // A click that beats the background plan (cold start) plans now, in
        // the same background task, instead of moving nothing.
        let owner = owner_home();
        let registry = self.provider_sessions.clone();
        self.close_dialog(cx);
        cx.spawn(async move |this, cx| {
            let report = cx
                .background_executor()
                .spawn(async move {
                    let plan = if plan.is_empty() { plan_migration(&owner, &state, &registry) } else { plan };
                    let pending = pending_moves(&state, plan);
                    execute_plan(&state, &pending)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                dismiss(&crate::store::support_dir());
                this.invalidate_migration_cache(cx);
                let done = report.moved + report.copied;
                let body = if report.errors.is_empty() {
                    format!("{done} moved into Baaz, {} already there.", report.skipped,)
                } else {
                    format!(
                        "{done} moved, {} failed — retry from Settings → Providers. First failure: {}",
                        report.errors.len(),
                        report.errors.first().cloned().unwrap_or_default(),
                    )
                };
                this.overlays.update(cx, |overlays, _| {
                    overlays.toast("Sessions moved", body);
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// The prompt's Not now: record the dismissal and close. The Providers
    /// row stays while moves remain, so the migration is always one click
    /// away; resume lazily moves whatever is left.
    pub(crate) fn dismiss_session_migration(&mut self, cx: &mut gpui::Context<Self>) {
        dismiss(&crate::store::support_dir());
        self.close_dialog(cx);
    }

    /// Flip the Providers row's path list open or shut.
    pub(crate) fn toggle_migration_paths(&mut self, cx: &mut gpui::Context<Self>) {
        self.migration_paths_expanded = !self.migration_paths_expanded;
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_sessions::{ProviderSessionStore, upsert_open};

    /// A temp owner HOME plus a temp state dir. Nothing here names the
    /// real `~/.claude`, `~/.codex` or Application Support dir.
    struct TempHomes {
        owner: PathBuf,
        state: PathBuf,
        _root: PathBuf,
    }

    impl TempHomes {
        fn make(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "baaz-migration-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_nanos())
                    .unwrap_or(0)
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("temp root");
            Self { owner: root.join("home"), state: root.join("state"), _root: root }
        }

        fn seed_claude(&self, slug: &str, session_id: &str, with_companion: bool) {
            let dir = self.owner.join(".claude").join("projects").join(slug);
            std::fs::create_dir_all(&dir).expect("slug dir");
            std::fs::write(dir.join(format!("{session_id}.jsonl")), "{\"id\":1}\n")
                .expect("transcript");
            if with_companion {
                let companion = dir.join(session_id);
                std::fs::create_dir_all(&companion).expect("companion dir");
                std::fs::write(companion.join("note.txt"), "sidecar").expect("companion file");
            }
        }

        fn seed_codex(&self, session_id: &str) {
            let day = self.owner.join(".codex").join("sessions").join("2026").join("10").join("02");
            std::fs::create_dir_all(&day).expect("date dir");
            std::fs::write(day.join(format!("rollout-2026-10-02-x-{session_id}.jsonl")), "{}\n")
                .expect("rollout");
        }
    }

    fn registry_with(claude: &[&str], codex: &[&str]) -> ProviderSessionStore {
        let mut store = ProviderSessionStore::new();
        for id in claude {
            upsert_open(&mut store, CLAUDE_PROVIDER, id, None, None, None);
        }
        for id in codex {
            upsert_open(&mut store, CODEX_PROVIDER, id, None, None, None);
        }
        // A muse record plans nothing, whatever its id.
        upsert_open(&mut store, "muse", "muse-1", None, None, None);
        store
    }

    #[test]
    fn the_plan_lists_only_registered_ids() {
        let _guard = serial();
        let homes = TempHomes::make("plan");
        homes.seed_claude("-work", "sess-1", true);
        homes.seed_claude("-work", "probe-leftover", false);
        homes.seed_codex("thread-1");
        std::fs::create_dir_all(homes.owner.join(".codex").join("sessions").join("2026"))
            .expect("date dir");
        std::fs::write(
            homes.owner.join(".codex").join("sessions").join("2026").join("rollout-x-probe-9.jsonl"),
            "{}\n",
        )
        .expect("unregistered rollout");

        let plan = plan_migration(&homes.owner, &homes.state, &registry_with(&["sess-1"], &["thread-1"]));
        let ids: Vec<(&str, bool)> =
            plan.iter().map(|planned| (planned.session_id.as_str(), planned.is_dir)).collect();
        assert!(ids.contains(&("sess-1", false)), "the registered transcript: {ids:?}");
        assert!(ids.contains(&("sess-1", true)), "its companion dir: {ids:?}");
        assert!(ids.contains(&("thread-1", false)), "the registered rollout: {ids:?}");
        assert_eq!(plan.len(), 3, "nothing else is listed: {plan:?}");
        assert!(
            !plan.iter().any(|planned| planned.session_id.contains("probe")),
            "unregistered probe leftovers stay where they are"
        );
        // Targets keep the same relative paths under the Baaz homes.
        assert!(
            plan.iter().all(|planned| planned.target.starts_with(&homes.state)),
            "every target lives under the state dir"
        );
        assert!(
            plan.iter().any(|planned| planned.target
                == homes.state.join("claude-home").join("projects").join("-work").join("sess-1.jsonl"))
        );
        assert!(
            plan.iter().any(|planned| planned.target
                == homes.state.join("codex-home").join("sessions").join("2026").join("10").join("02")
                    .join("rollout-2026-10-02-x-thread-1.jsonl"))
        );
    }

    #[test]
    fn already_moved_items_are_skipped() {
        let _guard = serial();
        let homes = TempHomes::make("idempotent-plan");
        homes.seed_claude("-work", "sess-1", false);
        homes.seed_codex("thread-1");
        // A previous run moved the transcript: target exists, source gone.
        let target = homes.state.join("claude-home").join("projects").join("-work").join("sess-1.jsonl");
        std::fs::create_dir_all(target.parent().expect("parent")).expect("target parent");
        std::fs::write(&target, "{\"id\":1}\n").expect("moved transcript");
        std::fs::remove_file(homes.owner.join(".claude").join("projects").join("-work").join("sess-1.jsonl"))
            .expect("source gone");

        let plan = plan_migration(&homes.owner, &homes.state, &registry_with(&["sess-1"], &["thread-1"]));
        assert_eq!(plan.len(), 1, "only the rollout still waits: {plan:?}");
        assert_eq!(plan[0].session_id, "thread-1");
    }

    #[test]
    fn the_executor_moves_and_a_second_run_is_a_no_op() {
        let _guard = serial();
        let homes = TempHomes::make("execute");
        homes.seed_claude("-work", "sess-1", true);
        homes.seed_codex("thread-1");
        // Unregistered files the run must never touch.
        homes.seed_claude("-work", "probe-leftover", false);

        let plan = plan_migration(&homes.owner, &homes.state, &registry_with(&["sess-1"], &["thread-1"]));
        assert_eq!(plan.len(), 3);
        let report = execute_plan(&homes.state, &plan);
        assert_eq!(report, MigrationReport { moved: 3, copied: 0, skipped: 0, errors: vec![] });

        // Sources left, targets hold identical bytes.
        for planned in &plan {
            assert!(!planned.source.exists(), "source gone: {}", planned.source.display());
            assert!(planned.target.exists(), "target holds it: {}", planned.target.display());
        }
        let transcript = homes.state.join("claude-home").join("projects").join("-work").join("sess-1.jsonl");
        assert_eq!(std::fs::read_to_string(&transcript).expect("reads"), "{\"id\":1}\n");
        let companion = homes.state.join("claude-home").join("projects").join("-work").join("sess-1").join("note.txt");
        assert_eq!(std::fs::read_to_string(&companion).expect("reads"), "sidecar");
        // The probe leftover is untouched, where it was.
        assert!(
            homes.owner.join(".claude").join("projects").join("-work").join("probe-leftover.jsonl").exists(),
            "unregistered files are never moved"
        );
        // The owner's Codex index is never written: no sqlite beside the moved rollout.
        assert!(
            std::fs::symlink_metadata(homes.owner.join(".codex").join("state_5.sqlite")).is_err()
                && std::fs::symlink_metadata(homes.state.join("codex-home").join("state_5.sqlite")).is_err(),
            "no sqlite file is created on either side"
        );

        // A second run finds nothing to do.
        let again = plan_migration(&homes.owner, &homes.state, &registry_with(&["sess-1"], &["thread-1"]));
        assert!(again.is_empty(), "already-moved items plan nothing");
        // The executor's own race guards, planner-filtered in practice.
        assert!(
            matches!(
                move_one(&plan[0].source, &plan[0].target, plan[0].is_dir),
                Ok(MoveOutcome::SkippedTargetExists)
            ),
            "a held target is never overwritten"
        );
        assert!(
            matches!(
                move_one(&homes.owner.join("gone.jsonl"), &homes.state.join("gone.jsonl"), false),
                Ok(MoveOutcome::SkippedSourceMissing)
            ),
            "a gone source moves nothing"
        );
        let report = execute_plan(&homes.state, &plan);
        assert_eq!(report, MigrationReport::default(), "re-running the old plan is a no-op");
        // And without the journal the target-exists skip still holds: the
        // plan itself is empty, so there is nothing to re-move.
        assert!(pending_moves(&homes.state, plan).is_empty());
    }

    #[test]
    fn the_journal_resumes_a_crash() {
        let _guard = serial();
        let homes = TempHomes::make("journal");
        homes.seed_claude("-work", "sess-1", false);
        homes.seed_codex("thread-1");
        let plan = plan_migration(&homes.owner, &homes.state, &registry_with(&["sess-1"], &["thread-1"]));
        assert_eq!(plan.len(), 2);

        // The crash between two moves: the first move's journal entry is
        // written, the second move never ran.
        let first = &plan[0];
        move_one(&first.source, &first.target, first.is_dir).expect("first move");
        mark_completed(&homes.state, &move_key(first), "moved");
        assert!(journal_path(&homes.state).exists(), "the journal records finished moves");

        // Resume skips the journaled move and runs the rest.
        let remaining = pending_moves(&homes.state, plan.clone());
        assert_eq!(remaining.len(), 1, "the finished move is not retried");
        assert_eq!(remaining[0].source, plan[1].source);
        let report = execute_plan(&homes.state, &remaining);
        assert_eq!(report.moved, 1);
        assert!(report.errors.is_empty());
        assert!(pending_moves(&homes.state, plan).is_empty(), "after the resume nothing pends");
    }

    #[test]
    fn the_copy_fallback_verifies_before_removing() {
        let _guard = serial();
        let homes = TempHomes::make("copy-verify");
        let source = homes.owner.join("file.jsonl");
        std::fs::create_dir_all(&homes.owner).expect("owner dir");
        std::fs::write(&source, "{\"id\":7}\n").expect("source");
        let target = homes.state.join("codex-home").join("sessions").join("file.jsonl");

        copy_and_verify(&source, &target, false).expect("verified copy");
        assert_eq!(
            std::fs::read(&target).expect("reads"),
            std::fs::read(&source).expect("reads"),
            "the copy reads back identical before any remove"
        );
        assert!(source.exists(), "copying never removes the source itself");
        let parent = target.parent().expect("parent");
        let entries: Vec<_> = std::fs::read_dir(parent)
            .expect("reads")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("file.jsonl")], "no staging residue: {entries:?}");
    }

    #[test]
    fn hostile_registry_ids_plan_nothing() {
        let _guard = serial();
        let homes = TempHomes::make("hostile");
        std::fs::create_dir_all(homes.owner.join(".claude").join("projects")).expect("projects");
        std::fs::create_dir_all(homes.owner.join(".codex").join("sessions")).expect("sessions");
        let plan = plan_migration(
            &homes.owner,
            &homes.state,
            &registry_with(&["../escape", "a/b"], &[".."]),
        );
        assert!(plan.is_empty(), "traversal ids never become moves: {plan:?}");
    }

    #[test]
    fn the_prompt_names_counts_paths_and_the_codex_note() {
        let _guard = serial();
        let homes = TempHomes::make("prompt");
        homes.seed_claude("-work", "sess-1", false);
        homes.seed_codex("thread-1");
        let plan = plan_migration(&homes.owner, &homes.state, &registry_with(&["sess-1"], &["thread-1"]));
        let (n_claude, n_codex) = counts(&plan);
        assert_eq!((n_claude, n_codex), (1, 1));
        assert_eq!(prompt_title(n_claude, n_codex), "Move 1 Claude Code and 1 Codex sessions into Baaz");
        let detail = prompt_detail(&plan);
        assert!(detail.contains("claude-home/projects/-work/sess-1.jsonl"), "dry-run paths: {detail}");
        assert!(detail.contains("codex-home/sessions/"), "dry-run paths: {detail}");
        assert!(detail.contains("archive"), "the Codex-for-Mac stale-entries note: {detail}");
        assert!(detail.contains("Nothing is deleted"), "the never-deletes note: {detail}");
    }

    #[test]
    fn not_now_dismisses_once_and_leaves_the_plan() {
        let _guard = serial();
        let homes = TempHomes::make("dismiss");
        assert!(!is_dismissed(&homes.state));
        dismiss(&homes.state);
        assert!(is_dismissed(&homes.state), "Not now records the dismissal");
        homes.seed_claude("-work", "sess-1", false);
        assert_eq!(
            plan_migration(&homes.owner, &homes.state, &registry_with(&["sess-1"], &[])).len(),
            1,
            "dismissal hides the prompt, never the moves"
        );
    }

    #[test]
    fn the_lazy_move_runs_the_same_executor_for_one_session() {
        let _guard = serial();
        let homes = TempHomes::make("lazy");
        homes.seed_claude("-work", "sess-1", true);
        homes.seed_codex("thread-1");

        let moved = ensure_session_moved(&homes.owner, &homes.state, CLAUDE_PROVIDER, "sess-1");
        assert_eq!(moved, 2, "transcript plus companion move on first resume");
        assert_eq!(
            ensure_session_moved(&homes.owner, &homes.state, CLAUDE_PROVIDER, "sess-1"),
            0,
            "the second resume finds nothing to move"
        );
        // The other provider's session waits for its own resume.
        assert!(
            homes.owner.join(".codex").join("sessions").join("2026").join("10").join("02")
                .join("rollout-2026-10-02-x-thread-1.jsonl")
                .exists(),
            "one session's resume never moves another's"
        );
        assert_eq!(ensure_session_moved(&homes.owner, &homes.state, "muse", "sess-1"), 0);
        assert_eq!(ensure_session_moved(&homes.owner, &homes.state, "unknown", "sess-1"), 0);
    }

    /// The cross-device branch and the walk counter are process-global, and
    /// this binary's tests run on shared threads: every test in this
    /// module holds this lock, so a hook set here never leaks into a
    /// rename-counting test next door (poison-tolerant: a failed test must
    /// not wedge the rest).
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Tests run on one filesystem, so a rename would succeed — the hook
    /// takes the cross-device copy leg instead. Production never sets it.
    struct ForceCopy;
    impl ForceCopy {
        fn on() -> Self {
            FORCE_CROSS_DEVICE_FOR_TESTS.store(true, std::sync::atomic::Ordering::Relaxed);
            ForceCopy
        }
    }
    impl Drop for ForceCopy {
        fn drop(&mut self) {
            FORCE_CROSS_DEVICE_FOR_TESTS.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// The hook's concurrent writer: one more byte lands between the copy
    /// and the remove, the way a racing write would.
    struct DirtyBetween;
    impl DirtyBetween {
        fn on() -> Self {
            DIRTY_BETWEEN_COPY_AND_REMOVE_FOR_TESTS.store(true, std::sync::atomic::Ordering::Relaxed);
            DirtyBetween
        }
    }
    impl Drop for DirtyBetween {
        fn drop(&mut self) {
            DIRTY_BETWEEN_COPY_AND_REMOVE_FOR_TESTS
                .store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }

    #[test]
    #[cfg(unix)]
    fn a_symlink_tree_is_refused_on_the_cross_device_path() {
        let _guard = serial();
        let homes = TempHomes::make("refuse-symlink");
        let source = homes.owner.join("tree");
        std::fs::create_dir_all(&source).expect("tree");
        std::fs::write(source.join("a.jsonl"), "{}\n").expect("file");
        std::os::unix::fs::symlink(source.join("a.jsonl"), source.join("link.jsonl"))
            .expect("symlink");
        let target = homes.state.join("codex-home").join("tree");

        let _force = ForceCopy::on();
        let error = move_one(&source, &target, true).expect_err("a symlink tree never moves");
        assert!(error.to_string().contains("refused"), "the reason names the refusal: {error}");
        assert!(source.exists(), "the source is untouched");
        assert!(
            source.join("link.jsonl").exists() || std::fs::symlink_metadata(source.join("link.jsonl")).is_ok(),
            "the symlink itself survives"
        );
        assert!(!target.exists(), "nothing lands at the target");
        // Through the executor the refusal is a user-visible report entry,
        // never a silent skip — and the journal records no done move.
        let planned = PlannedMove {
            provider: CODEX_PROVIDER.to_owned(),
            session_id: "thread-1".to_owned(),
            source: source.clone(),
            target: target.clone(),
            is_dir: true,
        };
        let report = execute_plan(&homes.state, &[planned]);
        assert_eq!(report.moved + report.copied + report.skipped, 0, "nothing moved: {report:?}");
        assert_eq!(report.errors.len(), 1, "one visible reason: {report:?}");
        assert!(report.errors[0].contains("refused"), "the reason: {}", report.errors[0]);
    }

    #[test]
    fn a_source_changed_mid_copy_keeps_both_copies() {
        let _guard = serial();
        let homes = TempHomes::make("changed-mid-copy");
        let source = homes.owner.join("file.jsonl");
        std::fs::create_dir_all(&homes.owner).expect("owner dir");
        std::fs::write(&source, "{\"id\":1}\n").expect("source");
        let target = homes.state.join("codex-home").join("file.jsonl");

        let _force = ForceCopy::on();
        let _dirty = DirtyBetween::on();
        let error = move_one(&source, &target, false).expect_err("a changed source never loses bytes");
        assert!(error.to_string().contains("kept both"), "the reason: {error}");
        assert!(source.exists(), "the newer source stays");
        assert!(target.exists(), "the copied target stays too");
        assert_ne!(
            std::fs::read(&source).expect("reads"),
            std::fs::read(&target).expect("reads"),
            "they genuinely differ — removing either would lose bytes"
        );
    }

    #[test]
    fn staging_has_no_pid_and_stale_staging_is_swept() {
        let _guard = serial();
        let homes = TempHomes::make("staging");
        let target = homes.state.join("codex-home").join("sessions").join("file.jsonl");
        let staging = staging_path(&target);
        assert_eq!(
            staging.file_name().expect("name").to_string_lossy(),
            ".file.jsonl.baaz-migrating",
            "no pid in the name, so any later run recognises the residue"
        );

        // Crash residue beside a waiting target: swept on the next run.
        std::fs::create_dir_all(staging.parent().expect("parent")).expect("parent");
        std::fs::write(&staging, "half a copy").expect("residue");
        let source = homes.owner.join("file.jsonl");
        std::fs::create_dir_all(&homes.owner).expect("owner dir");
        std::fs::write(&source, "{}\n").expect("source");
        let planned = PlannedMove {
            provider: CODEX_PROVIDER.to_owned(),
            session_id: "thread-1".to_owned(),
            source: source.clone(),
            target: target.clone(),
            is_dir: false,
        };
        let report = execute_plan(&homes.state, &[planned]);
        assert_eq!(report.moved, 1, "the move itself still runs: {report:?}");
        assert!(!staging.exists(), "the residue is gone");
        assert_eq!(std::fs::read(&target).expect("reads"), b"{}\n");
    }

    #[test]
    fn the_journal_retries_a_move_whose_target_is_gone() {
        let _guard = serial();
        let homes = TempHomes::make("reconcile");
        homes.seed_claude("-work", "sess-1", false);
        let plan = plan_migration(&homes.owner, &homes.state, &registry_with(&["sess-1"], &[]));
        assert_eq!(plan.len(), 1);
        // Journaled as done — but the target never landed while the source
        // still waits: the filesystem overrules the journal.
        mark_completed(&homes.state, &move_key(&plan[0]), "moved");
        let pending = pending_moves(&homes.state, plan.clone());
        assert_eq!(pending.len(), 1, "retried, not trusted: {pending:?}");
        let report = execute_plan(&homes.state, &pending);
        assert_eq!(report.moved, 1, "the retry lands it: {report:?}");
        assert!(pending_moves(&homes.state, plan).is_empty(), "then nothing pends");
    }

    #[test]
    fn the_journal_prunes_done_entries_older_than_30_days() {
        let _guard = serial();
        let homes = TempHomes::make("prune");
        let mut done = HashMap::new();
        done.insert(
            "codex/old".to_owned(),
            CompletedEntry { at: now_secs().saturating_sub(31 * 24 * 60 * 60), outcome: "moved" },
        );
        done.insert("codex/fresh".to_owned(), CompletedEntry { at: now_secs(), outcome: "copied" });
        write_journal(&homes.state, &done, &HashMap::new());

        prune_journal(&homes.state);
        let kept = read_completed(&homes.state);
        assert!(!kept.contains("codex/old"), "the 31-day entry is gone: {kept:?}");
        assert!(kept.contains("codex/fresh"), "the fresh entry stays: {kept:?}");
        // And the first version's list shape still parses (stamped fresh).
        std::fs::write(
            journal_path(&homes.state),
            "{\"completed\": [\"codex/legacy\"]}",
        )
        .expect("legacy journal");
        assert!(
            read_completed(&homes.state).contains("codex/legacy"),
            "legacy entries are honoured, not dropped"
        );
    }

    #[test]
    fn the_render_path_never_walks_the_homes() {
        // The spy is live: one planned walk moves it, the render accessor
        // must not.
        let _guard = serial();
        let homes = TempHomes::make("spy");
        homes.seed_claude("-work", "sess-1", false);
        let registry = registry_with(&["sess-1"], &[]);
        let before = PLAN_WALKS.load(std::sync::atomic::Ordering::Relaxed);
        let plan = plan_migration(&homes.owner, &homes.state, &registry);
        assert_eq!(
            PLAN_WALKS.load(std::sync::atomic::Ordering::Relaxed),
            before + 1,
            "the spy counts real walks"
        );
        let cache = MigrationCache::fresh(plan);
        let at_render = PLAN_WALKS.load(std::sync::atomic::Ordering::Relaxed);
        // What every render reads: the cached slice, twice, plus the empty
        // default a cold window renders before the background task lands.
        assert!(!cache.plan_for_render().is_empty());
        assert_eq!(cache.plan_for_render().len(), 1);
        assert!(MigrationCache::default().plan_for_render().is_empty());
        assert_eq!(
            PLAN_WALKS.load(std::sync::atomic::Ordering::Relaxed),
            at_render,
            "render reads walk nothing"
        );
    }
}
