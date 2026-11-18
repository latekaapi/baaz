//! Baaz's own on-disk state, next to Muse's rather than inside it.
//!
//! Muse owns `~/.config/muse` and `~/.local/share/muse`; Baaz never
//! writes to either. What Baaz has to remember — the billing tier it
//! last probed, the session names and hidden flags MSP has no
//! room for — lives under `~/Library/Application Support/baaz`,
//! which is the macOS answer for an application's own state.
//!
//! The per-turn usage ledger lives here too, as its own `baaz.db` (see
//! `crate::usage`), beside the search index rather than inside it.
//!
//! Two rules:
//!
//! * **Every write is atomic.** A file is written beside itself and renamed,
//!   so a crash mid-write leaves the previous state rather than half of the
//!   next one.
//! * **A damaged file is never wiped.** A missing file yields the default
//!   quietly. A file this build cannot parse (truncated, hand-edited, written
//!   by a newer build) is renamed aside to `<name>.corrupt-<ts>`, logged and
//!   reported ([`take_problems`] → one toast); the last good copy
//!   (`<name>.bak`, kept by [`write_authored`]) is read instead when there is
//!   one. A failed write is reported the same way and returns its error.

use std::path::PathBuf;

/// The state directory a run named with `BAAZ_STATE_DIR` — the one place
/// that reads the variable, shared with the provider crate's resolver
/// ([`provider::child_env::state_dir_override`]) so every caller agrees. An
/// empty value is unset: `BAAZ_STATE_DIR=""` used to write relative to the
/// working directory (docs/81 B D-9).
pub fn state_dir_override() -> Option<PathBuf> {
    provider::child_env::state_dir_override()
}

/// `~/Library/Application Support/baaz`, honouring `BAAZ_STATE_DIR` so a
/// test can point the whole store somewhere disposable: the run's
/// [`crate::runtime::StateDir`] (a capture with no state dir of its own gets
/// a throwaway one, a unit test a per-process temp dir).
pub fn support_dir() -> PathBuf {
    crate::runtime::state_dir().path
}

/// Removes this capture's throwaway store as the capture ends. Tier V makes one
/// per shot, and 1,079 had piled up before this. Only ever the capture dir:
/// a capture with its own `BAAZ_STATE_DIR` and every real run are untouched.
pub fn remove_capture_state() {
    let state = crate::runtime::state_dir();
    if state.throwaway {
        let _ = std::fs::remove_dir_all(state.path);
    }
}

/// Removes the throwaway stores of captures that died before they could
/// (`baaz-capture-state-<pid>` whose pid is gone). Off the UI thread.
pub fn sweep_dead_capture_states() {
    let _ = std::thread::Builder::new().name("capture-state-sweep".into()).spawn(|| {
        sweep_dead_capture_states_in(&std::env::temp_dir(), |pid| {
            // SAFETY: signal 0 only checks that the process exists.
            let gone = unsafe { libc::kill(pid, 0) } != 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            !gone
        });
    });
}

fn sweep_dead_capture_states_in(temp: &std::path::Path, alive: impl Fn(i32) -> bool) {
    let Ok(entries) = std::fs::read_dir(temp) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|name| name.strip_prefix("baaz-capture-state-")) else { continue };
        let Ok(pid) = pid.parse::<i32>() else { continue };
        if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) && !alive(pid) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// `~/Library/Application Support/baaz` regardless of `BAAZ_STATE_DIR`:
/// the directory the real app writes, which a scripted run with its own state
/// dir still has to recognise as Baaz's own (the tier probe's throwaway
/// workspace lives there and is never a project to adopt).
pub fn default_support_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join("Library").join("Application Support").join("baaz")
}

/// The pre-rename default, `~/Library/Application Support/harness`.
///
/// The name changed with the product (harness → Baaz); this path exists only
/// so [`migrate_legacy_support_dir`] can move it aside once. Nothing new is
/// ever written here.
pub fn legacy_default_support_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join("Library").join("Application Support").join("harness")
}

/// What [`migrate_support_dir_at`] did: the startup log prints it, and the
/// unit test asserts on it.
#[derive(Debug, PartialEq, Eq)]
pub enum StateMigration {
    /// Nothing to do: there is no old directory, or the new one already has
    /// everything the old one holds.
    NotNeeded,
    /// The old directory was renamed onto the new path.
    Moved,
    /// A rename was impossible (cross-device, permissions), so the tree was
    /// copied and the old directory removed afterwards.
    Copied,
    /// A new directory already existed - an early write beat the migration -
    /// so entries missing from it were filled in from the old one. Nothing in
    /// the new tree was overwritten, and the old directory is left in place.
    Merged,
}

/// One-time move of the pre-rename state directory onto the new path.
///
/// On startup, when `BAAZ_STATE_DIR` is unset (an explicit state dir means
/// the default paths are untouched), a `harness` directory is carried onto
/// the `baaz` path — sessions, projects, the search index, everything — so
/// existing state survives the rename. With no `baaz` directory the whole
/// tree moves; when one already exists, because an early write such as the
/// tier probe beat this call, only the entries it is missing are filled in
/// and nothing is overwritten. Never fails startup: every outcome is logged
/// and returned, and the app continues against whatever is on disk.
pub fn migrate_legacy_support_dir() -> StateMigration {
    if state_dir_override().is_some() {
        return StateMigration::NotNeeded;
    }
    let outcome = migrate_support_dir_at(&default_support_dir(), &legacy_default_support_dir());
    match &outcome {
        StateMigration::NotNeeded => {}
        StateMigration::Moved => {
            eprintln!(
                "baaz: migrated state {} -> {}",
                legacy_default_support_dir().display(),
                default_support_dir().display()
            );
        }
        StateMigration::Copied => {
            eprintln!(
                "baaz: copied state {} -> {} (rename impossible; old directory removed after a complete copy)",
                legacy_default_support_dir().display(),
                default_support_dir().display()
            );
        }
        StateMigration::Merged => {
            eprintln!(
                "baaz: filled missing state from {} into {} (the new directory already existed; nothing was overwritten, the old directory was kept)",
                legacy_default_support_dir().display(),
                default_support_dir().display()
            );
        }
    }
    outcome
}

/// The move itself, against explicit paths so the unit test never touches
/// `HOME` or the real `Application Support`.
fn migrate_support_dir_at(new: &std::path::Path, old: &std::path::Path) -> StateMigration {
    if !old.exists() {
        return StateMigration::NotNeeded;
    }
    if let Some(parent) = new.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return StateMigration::NotNeeded;
        }
    }
    // The clean case: nothing at the new path, so the whole tree moves.
    if !new.exists() {
        if std::fs::rename(old, new).is_ok() {
            return StateMigration::Moved;
        }
        if copy_dir_all(old, new).is_ok() && std::fs::remove_dir_all(old).is_ok() {
            return StateMigration::Copied;
        }
        return StateMigration::NotNeeded;
    }
    // The new directory already exists. It only takes one early write — a
    // tier probe, a crashed first launch — to create it before this runs,
    // and an all-or-nothing guard here would strand the old sessions
    // forever with no warning. Fill in whatever the new tree is missing and
    // leave everything it already has alone: the new copy is authoritative,
    // because the user has been running against it.
    if !copy_missing(old, new) {
        return StateMigration::NotNeeded;
    }
    StateMigration::Merged
}

/// Copy entries of `old` that have no counterpart in `new`, recursing into
/// directories that exist on both sides. Never overwrites. Returns whether
/// anything was copied.
fn copy_missing(old: &std::path::Path, new: &std::path::Path) -> bool {
    let Ok(entries) = std::fs::read_dir(old) else {
        return false;
    };
    let mut filled = false;
    for entry in entries.flatten() {
        let target = new.join(entry.file_name());
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            if target.exists() {
                filled |= copy_missing(&entry.path(), &target);
            } else if copy_dir_all(&entry.path(), &target).is_ok() {
                filled = true;
            }
        } else if !target.exists() && std::fs::copy(entry.path(), &target).is_ok() {
            filled = true;
        }
    }
    filled
}

/// Recursively copy a directory tree: `rename` cannot cross devices, and the
/// two support paths share a parent, but a symlinked or oddly-mounted home
/// still gets here.
fn copy_dir_all(old: &std::path::Path, new: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(new)?;
    for entry in std::fs::read_dir(old)? {
        let entry = entry?;
        let target = new.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// What a store met that the person should hear about (docs/81 A F8, F9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreProblem {
    /// A file could not be parsed and was renamed aside. `restored` says
    /// whether its `.bak` was read in its place.
    SetAside { name: String, aside: PathBuf, restored: bool },
    /// Entries of a map store that did not parse were dropped; the whole
    /// file was copied to `aside` first.
    EntriesDropped { name: String, count: usize, aside: PathBuf },
    /// A write failed; the file on disk is the previous one.
    WriteFailed { name: String, error: String },
}

static PROBLEMS: std::sync::Mutex<Vec<StoreProblem>> = std::sync::Mutex::new(Vec::new());
static PROBLEM_WAKERS: std::sync::Mutex<Vec<futures::channel::mpsc::UnboundedSender<()>>> =
    std::sync::Mutex::new(Vec::new());

fn report(problem: StoreProblem) {
    crate::baaz_log!("store: {problem:?}");
    PROBLEMS.lock().unwrap_or_else(|p| p.into_inner()).push(problem);
    PROBLEM_WAKERS.lock().unwrap_or_else(|p| p.into_inner()).retain(|waker| waker.unbounded_send(()).is_ok());
}

/// Every problem reported since the last call, oldest first.
pub fn take_problems() -> Vec<StoreProblem> {
    std::mem::take(&mut *PROBLEMS.lock().unwrap_or_else(|p| p.into_inner()))
}

/// A channel that receives one message per reported problem, so the window
/// raises its toast when a background write fails. Problems reported before
/// the window subscribed wait in [`take_problems`].
pub fn problem_wake() -> futures::channel::mpsc::UnboundedReceiver<()> {
    let (sender, receiver) = futures::channel::mpsc::unbounded();
    PROBLEM_WAKERS.lock().unwrap_or_else(|p| p.into_inner()).push(sender);
    receiver
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

/// `<path>.bak`: the previous good copy [`write_authored`] keeps.
pub fn backup_path(path: &std::path::Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".bak");
    PathBuf::from(name)
}

/// Write `bytes` to `path` through a sibling temporary file and a rename.
///
/// The parent directory is created if it is missing. The temporary name is
/// unique per call, so two writers of one path never share it; it is removed
/// when the write fails. The file and its directory are fsynced, so a crash
/// right after leaves either the old file or the new one. Blocking; call it
/// off the UI thread.
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".tmp{}-{n}", std::process::id()));
    let temporary = PathBuf::from(name);
    let written = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    if let Some(parent) = path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

/// [`write_atomic`] for a store the person authored (names, pins, projects,
/// layout, tabs): the current file is kept as `<name>.bak` first, and a
/// failure is reported ([`StoreProblem::WriteFailed`]) as well as returned,
/// so the caller can leave its generation where it was.
pub fn write_authored(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if path.exists() {
        let backup = backup_path(path);
        let _ = std::fs::remove_file(&backup);
        // A hard link costs nothing and keeps the old inode once the rename
        // replaces `path`; a copy where links are refused.
        if std::fs::hard_link(path, &backup).is_err() {
            let _ = std::fs::copy(path, &backup);
        }
    }
    let result = write_atomic(path, bytes);
    if let Err(error) = &result {
        report(StoreProblem::WriteFailed { name: file_name(path), error: error.to_string() });
    }
    result
}

/// Parse a JSON file in the store, or the type's default.
///
/// A missing file is the default, quietly. A file that does not parse is
/// renamed to `<name>.corrupt-<unix secs>` — never overwritten — and
/// reported; its `.bak` is read in its place when that parses.
pub fn read_json<T: serde::de::DeserializeOwned + Default>(path: &std::path::Path) -> T {
    read_migrated(path, |value| value)
}

/// [`read_json`] with a migration step between the parse and the type: the
/// file's JSON passes through `migrate` first, so an older layout can be
/// rewritten into the current one before it is read (the [`JsonStore`]
/// version hook). A file whose migrated value still does not fit `T` is set
/// aside like one that does not parse.
pub fn read_migrated<T: serde::de::DeserializeOwned + Default>(
    path: &std::path::Path,
    migrate: fn(serde_json::Value) -> serde_json::Value,
) -> T {
    let parse = |text: &str| -> Result<T, String> {
        let value: serde_json::Value = serde_json::from_str(text).map_err(|error| error.to_string())?;
        serde_json::from_value(migrate(value)).map_err(|error| error.to_string())
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return T::default(),
        Err(error) => {
            crate::baaz_log!("store: cannot read {}: {error}", path.display());
            return T::default();
        }
    };
    let error = match parse(&text) {
        Ok(value) => return value,
        Err(error) => error,
    };
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default();
    let mut aside = path.as_os_str().to_owned();
    aside.push(format!(".corrupt-{stamp}"));
    let aside = PathBuf::from(aside);
    crate::baaz_log!("store: {} does not parse ({error}); setting it aside", path.display());
    if std::fs::rename(path, &aside).is_err() {
        let _ = std::fs::copy(path, &aside);
    }
    let restored: Option<T> = std::fs::read_to_string(backup_path(path)).ok().and_then(|text| parse(&text).ok());
    report(StoreProblem::SetAside { name: file_name(path), aside, restored: restored.is_some() });
    restored.unwrap_or_default()
}

/// A field that falls back to its default when the stored value does not
/// parse (an enum variant a newer build added, a hand-edited slug), instead
/// of failing the whole file: `#[serde(default, deserialize_with =
/// "crate::store::lenient")]`.
pub fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    use serde::Deserialize as _;
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// [`read_json`] for a store that is a map of independent entries
/// (`sessions.json`, `provider-sessions.json`): one entry that does not
/// parse drops only itself, and the file is copied aside first so nothing
/// it said is lost.
pub fn read_json_map<V: serde::de::DeserializeOwned>(path: &std::path::Path) -> std::collections::HashMap<String, V> {
    let raw: serde_json::Map<String, serde_json::Value> = read_json(path);
    let mut kept = std::collections::HashMap::with_capacity(raw.len());
    let mut dropped = 0usize;
    for (key, value) in raw {
        match serde_json::from_value(value) {
            Ok(entry) => {
                kept.insert(key, entry);
            }
            Err(error) => {
                crate::baaz_log!("store: {} entry {key:?} does not parse ({error}); dropped", path.display());
                dropped += 1;
            }
        }
    }
    if dropped > 0 {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs())
            .unwrap_or_default();
        let mut aside = path.as_os_str().to_owned();
        aside.push(format!(".corrupt-{stamp}"));
        let aside = PathBuf::from(aside);
        let _ = std::fs::copy(path, &aside);
        report(StoreProblem::EntriesDropped { name: file_name(path), count: dropped, aside });
    }
    kept
}

/// One JSON file in the state directory, and the one way it is written
/// (docs/82 1.7, audit A R2).
///
/// Every store used to hand-roll its own ordering: a debounce epoch for
/// the pane state, another for the tabs, an in-flight counter for each
/// background writer the quit had to wait for, and three copies of a
/// generation check. One of them took its generation on the background
/// thread, after the snapshot, so a later snapshot could lose to an
/// earlier one (docs/81 A F10). A `JsonStore` owns all of that:
///
/// * [`Self::save`] / [`Self::save_soon`] take the generation and resolve
///   the path **when the snapshot is taken**, on the caller's thread, and
///   hand the value to the store's one writer thread. Newest wins: a
///   snapshot queued behind a newer one is never written.
/// * `save_soon` waits the store's delay first, so a burst (Files
///   navigation, tab titles) coalesces into one write.
/// * [`Self::write_now`] writes on the caller's thread, ordered against the
///   writer; [`Self::flush`] blocks until everything queued is on disk (the
///   quit and window-close durability point).
/// * Writes go through [`write_authored`] (`.bak` first, a failure
///   reported); reads through [`read_migrated`] (quarantine on a bad parse,
///   the `.bak` read instead, and the store's migration hook).
pub struct JsonStore<T> {
    /// The path under the state directory (`"tabs.json"`,
    /// `"Browser/tabs.json"`), resolved against [`support_dir`] per call so
    /// a run that names its own `BAAZ_STATE_DIR` is honoured.
    name: &'static str,
    /// How long [`Self::save_soon`] waits for more changes (750 ms unless
    /// the store says).
    delay: std::time::Duration,
    /// The version hook: the file's JSON before it is read as `T`.
    migrate: fn(serde_json::Value) -> serde_json::Value,
    /// How the value becomes bytes (pretty JSON unless the store says).
    encode: fn(&T) -> Option<Vec<u8>>,
    /// The next snapshot's generation.
    next: std::sync::atomic::AtomicU64,
    queue: std::sync::Mutex<Queue<T>>,
    changed: std::sync::Condvar,
    writer: std::sync::OnceLock<()>,
}

struct Queue<T> {
    /// The newest snapshot not yet written, one per path. A store has one
    /// path in a run; tests point `BAAZ_STATE_DIR` at several, and one
    /// test's write must never stand for another's.
    pending: Vec<Pending<T>>,
    /// The newest generation on disk, per path.
    written: Vec<(PathBuf, u64)>,
    /// The writer holds a snapshot it took off the queue.
    writing: bool,
}

impl<T> Queue<T> {
    fn written(&self, path: &std::path::Path) -> u64 {
        self.written.iter().find(|(written, _)| written == path).map(|(_, generation)| *generation).unwrap_or(0)
    }

    fn mark_written(&mut self, path: &std::path::Path, generation: u64) {
        match self.written.iter_mut().find(|(written, _)| written == path) {
            Some((_, newest)) => *newest = (*newest).max(generation),
            None => self.written.push((path.to_owned(), generation)),
        }
    }
}

struct Pending<T> {
    generation: u64,
    path: PathBuf,
    value: T,
    /// Not before this instant ([`JsonStore::save_soon`]).
    due: std::time::Instant,
}

fn pretty<T: serde::Serialize>(value: &T) -> Option<Vec<u8>> {
    serde_json::to_vec_pretty(value).ok()
}

fn identity(value: serde_json::Value) -> serde_json::Value {
    value
}

impl<T> JsonStore<T>
where
    T: serde::Serialize + serde::de::DeserializeOwned + Default + Send + 'static,
{
    /// A store for `name` under the state directory.
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            delay: std::time::Duration::from_millis(750),
            migrate: identity,
            encode: pretty::<T>,
            next: std::sync::atomic::AtomicU64::new(1),
            queue: std::sync::Mutex::new(Queue { pending: Vec::new(), written: Vec::new(), writing: false }),
            changed: std::sync::Condvar::new(),
            writer: std::sync::OnceLock::new(),
        }
    }

    /// The version hook: rewrite an older file's JSON before it is read.
    pub const fn with_migration(mut self, migrate: fn(serde_json::Value) -> serde_json::Value) -> Self {
        self.migrate = migrate;
        self
    }

    /// How the value becomes bytes, for a store that keeps only part of it
    /// (`None` writes nothing).
    pub const fn with_encoding(mut self, encode: fn(&T) -> Option<Vec<u8>>) -> Self {
        self.encode = encode;
        self
    }

    /// The file's path now.
    pub fn path(&self) -> PathBuf {
        support_dir().join(self.name)
    }

    /// The stored value, or the default ([`read_migrated`]). Blocking.
    pub fn read(&self) -> T {
        read_migrated(&self.path(), self.migrate)
    }

    /// Queue `value` for the writer thread, written as soon as it gets to it.
    pub fn save(&'static self, value: T) {
        self.enqueue(value, std::time::Duration::ZERO);
    }

    /// Queue `value` to be written after the store's delay, unless a newer
    /// snapshot replaces it first. A change already waiting keeps its
    /// earlier due time, so a constant stream still lands every delay.
    pub fn save_soon(&'static self, value: T) {
        self.enqueue(value, self.delay);
    }

    /// Write `value` on this thread, ordered after every snapshot taken
    /// before it. Blocking.
    pub fn write_now(&self, value: &T) -> std::io::Result<()> {
        let generation = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = self.path();
        let mut queue = self.lock();
        while queue.writing {
            queue = self.changed.wait(queue).unwrap_or_else(|p| p.into_inner());
        }
        // A snapshot queued for this file is older than this one: drop it.
        queue.pending.retain(|pending| pending.path != path || pending.generation > generation);
        let result = self.write_bytes(&path, value);
        if result.is_ok() {
            queue.mark_written(&path, generation);
        }
        self.changed.notify_all();
        result
    }

    /// Block until every queued snapshot is on disk (or failed).
    pub fn flush(&self) {
        let mut queue = self.lock();
        let now = std::time::Instant::now();
        for pending in &mut queue.pending {
            pending.due = now;
        }
        self.changed.notify_all();
        while !queue.pending.is_empty() || queue.writing {
            queue = self.changed.wait(queue).unwrap_or_else(|p| p.into_inner());
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Queue<T>> {
        self.queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write_bytes(&self, path: &std::path::Path, value: &T) -> std::io::Result<()> {
        match (self.encode)(value) {
            Some(bytes) => write_authored(path, &bytes),
            None => Ok(()),
        }
    }

    fn enqueue(&'static self, value: T, wait: std::time::Duration) {
        let generation = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = self.path();
        let due = std::time::Instant::now() + wait;
        {
            let mut queue = self.lock();
            match queue.pending.iter_mut().find(|pending| pending.path == path) {
                // Newest wins; a change already waiting keeps its due time.
                Some(older) => {
                    older.generation = generation;
                    older.value = value;
                    older.due = older.due.min(due);
                }
                None => queue.pending.push(Pending { generation, path, value, due }),
            }
            self.changed.notify_all();
        }
        self.writer.get_or_init(|| self.start_writer());
    }

    fn start_writer(&'static self) {
        let spawned = std::thread::Builder::new()
            .name(format!("store:{}", self.name))
            .spawn(move || self.run_writer());
        if let Err(error) = spawned {
            crate::baaz_log!("store: no writer for {}: {error}", self.name);
        }
    }

    fn run_writer(&'static self) {
        let mut queue = self.lock();
        loop {
            let next = queue
                .pending
                .iter()
                .enumerate()
                .min_by_key(|(_, pending)| pending.due)
                .map(|(ix, pending)| (ix, pending.due));
            let Some((ix, due)) = next else {
                queue = self.changed.wait(queue).unwrap_or_else(|p| p.into_inner());
                continue;
            };
            let now = std::time::Instant::now();
            if due > now {
                queue = self.changed.wait_timeout(queue, due - now).unwrap_or_else(|p| p.into_inner()).0;
                continue;
            }
            let pending = queue.pending.remove(ix);
            if pending.generation <= queue.written(&pending.path) {
                self.changed.notify_all();
                continue;
            }
            queue.writing = true;
            drop(queue);
            let result = self.write_bytes(&pending.path, &pending.value);
            queue = self.lock();
            queue.writing = false;
            if result.is_ok() {
                queue.mark_written(&pending.path, pending.generation);
            }
            self.changed.notify_all();
        }
    }
}

/// Serializes the tests that point `BAAZ_STATE_DIR` at a temp dir: two
/// tests pointing it at two dirs at once would read each other's state.
///
/// Returns the guard directly, poison-tolerant on purpose: a test that
/// panics while holding the guard poisons a plain `Mutex`, and every later
/// `.lock().expect(..)` dies with `PoisonError` — a cascade, not a signal.
/// Recovering the guard keeps one test's failure from failing the suite.
#[cfg(any(test, feature = "test-support"))]
pub fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    static ORDERED: JsonStore<Vec<u32>> = JsonStore::new("ordered.json");

    fn in_temp_state(tag: &str) -> (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, PathBuf) {
        let guard = test_env_lock();
        let old = std::env::var_os("BAAZ_STATE_DIR");
        let dir = std::env::temp_dir().join(format!("baaz-json-store-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        (guard, old, dir)
    }

    fn restore(old: Option<std::ffi::OsString>, dir: PathBuf) {
        match old {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// docs/81 A F10: a snapshot taken before a newer one never lands over
    /// it, however the writes are timed.
    #[test]
    fn an_older_snapshot_never_wins_the_file() {
        let (_guard, old, dir) = in_temp_state("order");
        ORDERED.save_soon(vec![1]);
        ORDERED.write_now(&vec![2]).expect("the newer snapshot writes");
        ORDERED.flush();
        assert_eq!(ORDERED.read(), vec![2], "the queued older snapshot was dropped");
        // Several quick saves coalesce; the newest lands.
        ORDERED.save_soon(vec![3]);
        ORDERED.save_soon(vec![4]);
        ORDERED.save(vec![5]);
        ORDERED.flush();
        assert_eq!(ORDERED.read(), vec![5]);
        restore(old, dir);
    }

    /// A delayed save lands on its own once the delay passes.
    #[test]
    fn a_delayed_save_lands_without_a_flush() {
        let (_guard, old, dir) = in_temp_state("delay");
        ORDERED.save_soon(vec![7]);
        assert!(!ORDERED.path().exists(), "nothing is written before the delay");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while ORDERED.read() != vec![7] && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(ORDERED.read(), vec![7]);
        restore(old, dir);
    }

    /// Two state directories (two tests, or a test and a capture) never
    /// stand for each other: a write to one does not drop the other's.
    #[test]
    fn each_file_keeps_its_own_order() {
        let (_guard, old, dir) = in_temp_state("paths-a");
        ORDERED.save_soon(vec![10]);
        let other = std::env::temp_dir().join(format!("baaz-json-store-paths-b-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&other);
        std::env::set_var("BAAZ_STATE_DIR", &other);
        ORDERED.write_now(&vec![20]).expect("write");
        ORDERED.flush();
        assert_eq!(ORDERED.read(), vec![20]);
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        assert_eq!(ORDERED.read(), vec![10], "the first directory's save still landed");
        let _ = std::fs::remove_dir_all(&other);
        restore(old, dir);
    }

    /// The version hook runs between the parse and the type.
    #[test]
    fn the_migration_hook_reads_an_older_layout() {
        fn wrap(value: serde_json::Value) -> serde_json::Value {
            match value {
                serde_json::Value::Object(_) => value,
                bare => serde_json::json!({ "items": bare }),
            }
        }
        #[derive(Default, serde::Deserialize, PartialEq, Debug)]
        struct Current {
            items: Vec<u32>,
        }
        let dir = std::env::temp_dir().join(format!("baaz-migration-hook-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.json");
        std::fs::write(&path, "[1,2]").unwrap();
        let read: Current = read_migrated(&path, wrap);
        assert_eq!(read, Current { items: vec![1, 2] });
        assert!(path.exists(), "a file the hook can read is never set aside");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rename_replaces_the_previous_contents_and_leaves_no_temporary() {
        let dir = std::env::temp_dir().join(format!("baaz-store-{}", std::process::id()));
        let path = dir.join("thing.json");
        write_atomic(&path, b"{\"a\":1}").expect("first write");
        write_atomic(&path, b"{\"a\":2}").expect("second write");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":2}");
        let leftovers = std::fs::read_dir(&dir).unwrap().filter_map(Result::ok).count();
        assert_eq!(leftovers, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// docs/81 A F8: a damaged file is set aside, never overwritten, and
    /// reported; with no `.bak` the read is the default.
    #[test]
    fn an_unparseable_file_is_set_aside_and_reported() {
        let _guard = test_env_lock();
        let dir = std::env::temp_dir().join(format!("baaz-store-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("thing.json");
        write_atomic(&path, b"{\"a\": 1, \"trunc").expect("write");
        let _ = take_problems();
        assert_eq!(read_json::<serde_json::Map<String, serde_json::Value>>(&path).len(), 0);
        assert!(!path.exists(), "the damaged file is moved, so no later write can replace it");
        let aside: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("thing.json.corrupt-"))
            .collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(std::fs::read(aside[0].path()).unwrap(), b"{\"a\": 1, \"trunc");
        let problems = take_problems();
        assert!(
            problems.iter().any(|p| matches!(p, StoreProblem::SetAside { name, restored: false, .. } if name == "thing.json")),
            "{problems:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The last good copy is read when the file is damaged.
    #[test]
    fn a_damaged_file_falls_back_to_its_backup() {
        let _guard = test_env_lock();
        let dir = std::env::temp_dir().join(format!("baaz-store-bak-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("kept.json");
        write_authored(&path, b"{\"a\":1}").expect("first");
        write_authored(&path, b"{\"a\":2}").expect("second");
        assert_eq!(std::fs::read(backup_path(&path)).unwrap(), b"{\"a\":1}", "one .bak: the previous write");
        std::fs::write(&path, b"").expect("truncate");
        let back: serde_json::Map<String, serde_json::Value> = read_json(&path);
        assert_eq!(back.get("a"), Some(&serde_json::json!(1)), "read from the .bak");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// docs/81 A F9: a failed write returns its error, leaves no temporary
    /// and is reported.
    #[test]
    fn a_failed_write_is_reported_and_leaves_no_temporary() {
        let _guard = test_env_lock();
        let dir = std::env::temp_dir().join(format!("baaz-store-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // A directory where the file should be: the rename fails.
        let path = dir.join("blocked.json");
        std::fs::create_dir_all(path.join("inside")).expect("seed");
        let _ = take_problems();
        assert!(write_authored(&path, b"{}").is_err());
        let names: Vec<String> =
            std::fs::read_dir(&dir).unwrap().filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert!(names.iter().all(|name| !name.contains(".tmp")), "{names:?}");
        assert!(take_problems().iter().any(|p| matches!(p, StoreProblem::WriteFailed { name, .. } if name == "blocked.json")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_legacy_state_dir_moves_to_the_new_path_once() {
        let base = std::env::temp_dir().join(format!("baaz-migrate-{}", std::process::id()));
        let old = base.join("harness");
        let new = base.join("baaz");
        let _ = std::fs::remove_dir_all(&base);
        // No old dir, no new dir: nothing to do.
        assert_eq!(migrate_support_dir_at(&new, &old), StateMigration::NotNeeded);
        // An old dir with sessions, projects and a nested database moves whole.
        std::fs::create_dir_all(old.join("nested")).expect("seed old");
        std::fs::write(old.join("sessions.json"), b"{}").expect("seed sessions");
        std::fs::write(old.join("nested").join("search.db"), b"db").expect("seed db");
        assert_eq!(migrate_support_dir_at(&new, &old), StateMigration::Moved);
        assert!(!old.exists(), "the old path is gone after the move");
        assert_eq!(std::fs::read_to_string(new.join("sessions.json")).unwrap(), "{}");
        assert_eq!(std::fs::read(new.join("nested").join("search.db")).unwrap(), b"db");
        // A second run is a no-op: the new dir already exists.
        assert_eq!(migrate_support_dir_at(&new, &old), StateMigration::NotNeeded);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The regression this guards: a tier probe once wrote
    /// `baaz/tier-probe` before any launch could migrate, and the old
    /// `new.exists()` guard then refused forever — 73 sessions stranded in
    /// `harness/sessions.json` with no warning. A partial new directory must
    /// be filled in, not treated as done.
    #[test]
    fn an_early_write_to_the_new_dir_does_not_strand_the_old_state() {
        let base = std::env::temp_dir().join(format!("baaz-merge-{}", std::process::id()));
        let old = base.join("harness");
        let new = base.join("baaz");
        let _ = std::fs::remove_dir_all(&base);
        // The old tree: the user's real state.
        std::fs::create_dir_all(old.join("nested")).expect("seed old");
        std::fs::write(old.join("sessions.json"), b"OLD-SESSIONS").expect("seed sessions");
        std::fs::write(old.join("layout.json"), b"OLD-LAYOUT").expect("seed layout");
        std::fs::write(old.join("nested").join("search.db"), b"OLD-DB").expect("seed db");
        // An early write beat the migration: the new dir exists with one file,
        // and one file that also exists on the old side with newer content.
        std::fs::create_dir_all(&new).expect("seed new");
        std::fs::write(new.join("tier-probe"), b"PROBE").expect("seed probe");
        std::fs::write(new.join("layout.json"), b"NEW-LAYOUT").expect("seed new layout");

        assert_eq!(migrate_support_dir_at(&new, &old), StateMigration::Merged);
        // What the new tree lacked is filled in, including nested trees.
        assert_eq!(std::fs::read(new.join("sessions.json")).unwrap(), b"OLD-SESSIONS");
        assert_eq!(std::fs::read(new.join("nested").join("search.db")).unwrap(), b"OLD-DB");
        // What it already had is untouched: the user has been running against it.
        assert_eq!(std::fs::read(new.join("layout.json")).unwrap(), b"NEW-LAYOUT");
        assert_eq!(std::fs::read(new.join("tier-probe")).unwrap(), b"PROBE");
        // The old directory is kept, so a merge is never a destructive step.
        assert!(old.exists(), "a merge leaves the old directory in place");
        // Running again finds nothing missing and reports so.
        assert_eq!(migrate_support_dir_at(&new, &old), StateMigration::NotNeeded);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A test that panics while holding the env lock must not fail every
    /// later env-guarded test with `PoisonError`: the lock recovers, so one
    /// failure stays one failure instead of cascading across the suite.
    #[test]
    fn a_panicking_test_does_not_poison_the_next_tests_lock() {
        let probe = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = super::test_env_lock();
            panic!("a failing test panics while holding the env lock");
        }));
        assert!(probe.is_err(), "the probe must really panic while holding the lock");
        // The next acquisition recovers instead of dying with PoisonError.
        let _guard = super::test_env_lock();
    }
}

#[cfg(test)]
mod isolation_tests {
    /// Without `BAAZ_STATE_DIR`, a test build's store is a temp dir, never
    /// `~/Library/Application Support/baaz`.
    #[test]
    fn an_unset_state_dir_never_resolves_to_the_real_support_dir_in_tests() {
        let _guard = super::test_env_lock();
        let old = std::env::var_os("BAAZ_STATE_DIR");
        std::env::remove_var("BAAZ_STATE_DIR");
        let dir = super::support_dir();
        if let Some(value) = old {
            std::env::set_var("BAAZ_STATE_DIR", value);
        }
        assert_ne!(dir, super::default_support_dir());
        assert!(dir.starts_with(std::env::temp_dir()), "{dir:?}");
    }

    #[test]
    fn only_dead_capture_states_are_swept() {
        let temp = std::env::temp_dir().join(format!("baaz-capsweep-{}", std::process::id()));
        for name in ["baaz-capture-state-111", "baaz-capture-state-222", "baaz-capture-state-x", "baaz-other-111"] {
            std::fs::create_dir_all(temp.join(name)).unwrap();
        }
        super::sweep_dead_capture_states_in(&temp, |pid| pid == 222);
        assert!(!temp.join("baaz-capture-state-111").exists(), "dead");
        assert!(temp.join("baaz-capture-state-222").exists(), "alive");
        assert!(temp.join("baaz-capture-state-x").exists(), "not a pid");
        assert!(temp.join("baaz-other-111").exists(), "not ours");
        let _ = std::fs::remove_dir_all(&temp);
    }
}
