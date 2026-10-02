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
pub fn owner_home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

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

/// The finished keys the journal holds. Best-effort like every store
/// read: a missing or unparseable journal is empty, and the target-exists
/// skip below keeps the executor idempotent regardless.
fn read_completed(state_dir: &Path) -> HashSet<String> {
    let text = std::fs::read_to_string(journal_path(state_dir)).unwrap_or_default();
    let parsed: HashMap<String, Vec<String>> = serde_json::from_str(&text).unwrap_or_default();
    parsed.get("completed").cloned().unwrap_or_default().into_iter().collect()
}

/// Record one finished move in the journal, atomically. Best-effort: a
/// journal that cannot be written loses a resume hint, never a session
/// (the target-exists skip still recognises the finished move).
fn mark_completed(state_dir: &Path, key: &str) {
    let mut done = read_completed(state_dir);
    if !done.insert(key.to_owned()) {
        return;
    }
    let mut ordered: Vec<&String> = done.iter().collect();
    ordered.sort();
    let body = HashMap::from([("completed".to_owned(), ordered)]);
    if let Ok(text) = serde_json::to_vec_pretty(&body) {
        let _ = crate::store::write_atomic(&journal_path(state_dir), &text);
    }
}

/// Drop the moves the journal already records: the crash-resume filter.
/// The target-exists skip in [`plan_migration`] already handles the
/// finished-and-journaled case; this covers a journal written for a move
/// whose target check races it.
pub fn pending_moves(state_dir: &Path, moves: Vec<PlannedMove>) -> Vec<PlannedMove> {
    let done = read_completed(state_dir);
    moves.into_iter().filter(|planned| !done.contains(&move_key(planned))).collect()
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

/// Copy one file or directory tree onto `target`, verifying before the
/// caller removes the source: every byte must read back identical, or the
/// source stays and the error propagates. Directories copy through a
/// sibling temporary (never a partial target), then commit by rename.
fn copy_and_verify(source: &Path, target: &Path, is_dir: bool) -> std::io::Result<()> {
    // A stale temporary from a crashed copy is residue, not data: clear it.
    let staging = target.with_extension(format!("migrating{}", std::process::id()));
    if let Some(parent) = staging.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if staging.exists() {
        if staging.is_dir() {
            std::fs::remove_dir_all(&staging)?;
        } else {
            std::fs::remove_file(&staging)?;
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

/// Move one source onto its target: `rename`, falling back to
/// copy-plus-verify-plus-remove only across devices. Nothing else is ever
/// removed — a failed or unverified copy leaves the source in place and
/// reports the error. The target's parent directories are created; an
/// existing target (or a missing source) is a skip, never an overwrite.
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
    match std::fs::rename(source, target) {
        Ok(()) => Ok(MoveOutcome::Moved),
        Err(error) if is_cross_device(&error) => {
            copy_and_verify(source, target, is_dir)?;
            if is_dir {
                std::fs::remove_dir_all(source)?;
            } else {
                std::fs::remove_file(source)?;
            }
            Ok(MoveOutcome::CopiedAcrossDevices)
        }
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
/// crash resumes where it stopped. Skips (gone sources, held targets) and
/// failures never journal: skips need no resume, failures must retry.
pub fn execute_plan(state_dir: &Path, moves: &[PlannedMove]) -> MigrationReport {
    let mut report = MigrationReport::default();
    for planned in pending_moves(state_dir, moves.to_vec()) {
        match move_one(&planned.source, &planned.target, planned.is_dir) {
            Ok(MoveOutcome::Moved) => {
                report.moved += 1;
                mark_completed(state_dir, &move_key(&planned));
            }
            Ok(MoveOutcome::CopiedAcrossDevices) => {
                report.copied += 1;
                mark_completed(state_dir, &move_key(&planned));
            }
            Ok(MoveOutcome::SkippedTargetExists | MoveOutcome::SkippedSourceMissing) => {
                report.skipped += 1;
            }
            Err(error) => report.errors.push(format!("{}: {error}", planned.target.display())),
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

impl BaazHarness {
    /// This window's migration plan from its own registry: owner-side
    /// files still waiting under the real `HOME`, targeting this run's
    /// state dir (which honours `BAAZ_STATE_DIR`).
    pub(crate) fn migration_plan(&self) -> Vec<PlannedMove> {
        plan_migration(&owner_home(), &crate::store::support_dir(), &self.provider_sessions)
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
    /// connect screen, or a deterministic capture.
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
        let plan = self.migration_plan();
        if plan.is_empty() {
            return;
        }
        self.set_dialog(cx, Self::migration_dialog(&plan));
    }

    /// Run the prompt's Move: the full plan through the journaling
    /// executor, then a toast with the honest counts (moved, already
    /// there, failed). The prompt never shows again afterwards.
    pub(crate) fn confirm_session_migration(&mut self, cx: &mut gpui::Context<Self>) {
        let state = crate::store::support_dir();
        let plan = self.migration_plan();
        let report = execute_plan(&state, &plan);
        dismiss(&state);
        self.close_dialog(cx);
        let done = report.moved + report.copied;
        let body = if report.errors.is_empty() {
            format!(
                "{done} moved into Baaz, {} already there.",
                report.skipped,
            )
        } else {
            format!(
                "{done} moved, {} failed — retry from Settings → Providers. First failure: {}",
                report.errors.len(),
                report.errors.first().cloned().unwrap_or_default(),
            )
        };
        self.overlays.update(cx, |overlays, _| {
            overlays.toast("Sessions moved", body);
        });
        cx.notify();
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
        let homes = TempHomes::make("journal");
        homes.seed_claude("-work", "sess-1", false);
        homes.seed_codex("thread-1");
        let plan = plan_migration(&homes.owner, &homes.state, &registry_with(&["sess-1"], &["thread-1"]));
        assert_eq!(plan.len(), 2);

        // The crash between two moves: the first move's journal entry is
        // written, the second move never ran.
        let first = &plan[0];
        move_one(&first.source, &first.target, first.is_dir).expect("first move");
        mark_completed(&homes.state, &move_key(first));
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
}
