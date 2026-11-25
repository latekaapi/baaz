//! `baaz.db`: Baaz's one database (docs/82 4.12, docs/81 B §c.2).
//!
//! Every fact Baaz keeps about a session lives in one SQLite file under the
//! state dir: the session row (keyed by provider and the provider's own id,
//! with Baaz's own uuid beside it), its turns (the usage ledger, the bubble
//! text a submit showed), what each turn was sent with, the prompt history,
//! the import and migration journals. Before this, those facts were eight
//! JSON files keyed by a bare session id, rewritten whole on every change.
//!
//! * **One writer.** One thread, `baaz.db`, owns every connection (one per
//!   state dir a run names; tests point several at once). Work reaches it
//!   as a job, run in the order it was queued: [`run`] queues and returns,
//!   [`call`] queues and waits for the answer, [`flush`] waits for every
//!   job queued before it. Nothing else opens the file.
//! * **The UI thread never touches SQLite.** Every connection access
//!   asserts [`check_off_ui`] (the assert `browser.db`'s worker has used
//!   since 4.5a) and that it runs on the writer thread.
//! * **Migrations go through `PRAGMA user_version`**, each step one
//!   transaction ([`schema`]). The file is in WAL mode.
//! * **Multi-row writes are one transaction**: a snapshot of the session
//!   list, a journal, a sent turn, the first-launch import.
//! * **First launch imports the JSON stores** ([`json_import`]) in one
//!   transaction, then marks the database imported in the same
//!   transaction. The JSON files are never renamed, rewritten or deleted:
//!   they stay as the backup. A failed import leaves nothing behind and the
//!   database unopened for that run; the next launch tries again.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Mutex, OnceLock};

use rusqlite::Connection;

pub mod attachments;
pub mod history;
pub mod integrity;
pub mod journals;
pub mod json_import;
pub mod listing;
pub mod schema;
pub mod sessions;
pub mod tasks;
pub mod turns;
pub mod worktrees;

thread_local! {
    /// Set on the thread that runs the window: the UI thread
    /// ([`mark_ui_thread`]).
    pub static UI_THREAD: Cell<bool> = const { Cell::new(false) };
    /// Set while a worker job runs, on whatever thread runs it (the
    /// browser's worker runs on the background executor, which is the test
    /// thread under gpui's test executor).
    pub static IN_JOB: Cell<bool> = const { Cell::new(false) };
    /// Set on this module's writer thread.
    static DB_THREAD: Cell<bool> = const { Cell::new(false) };
}

/// Say that the calling thread runs the window. [`check_off_ui`] fails on it
/// from then on, outside a worker job.
pub fn mark_ui_thread() {
    UI_THREAD.with(|ui| ui.set(true));
}

/// Every SQLite call on Baaz's own files asserts this: SQLite never runs on
/// the UI thread outside a worker job. Checked in tests and debug builds.
pub fn check_off_ui(what: &str) {
    if cfg!(any(test, debug_assertions)) {
        let on_ui = UI_THREAD.with(Cell::get) && !IN_JOB.with(Cell::get);
        assert!(!on_ui, "SQLite: `{what}` ran on the UI thread outside a worker job");
    }
}

/// The database's file name under the state dir.
pub const FILE_NAME: &str = "baaz.db";

/// `<state dir>/baaz.db` for this run.
pub fn path() -> PathBuf {
    path_in(&crate::store::support_dir())
}

/// `baaz.db` under `state_dir`.
pub fn path_in(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE_NAME)
}

/// One open database on the writer thread: its connection, and what the
/// window's session list last wrote to it ([`sessions::FrontState`]), so a
/// snapshot writes only the rows that changed.
pub struct Opened {
    pub conn: Connection,
    pub(crate) front: sessions::FrontState,
    last_used: u64,
}

/// A database the writer lets go of runs `PRAGMA optimize` first (docs/85
/// 10.8): SQLite's own advice, cheap, and the statistics help the planner
/// next time.
impl Drop for Opened {
    fn drop(&mut self) {
        let _ = self.conn.execute_batch("PRAGMA optimize;");
    }
}

/// One statement through the connection's statement cache (docs/85 10.8):
/// the same SQL is parsed once per connection, not on every call.
pub(crate) fn exec<P: rusqlite::Params>(conn: &Connection, sql: &str, params: P) -> rusqlite::Result<usize> {
    conn.prepare_cached(sql)?.execute(params)
}

/// The writer checkpoints and truncates a database's WAL every this many
/// uses (docs/85 10.8), so the WAL never grows between quits.
const CHECKPOINT_EVERY: u64 = 500;

/// The writer thread's state: the databases it holds open.
#[derive(Default)]
struct Worker {
    open: HashMap<PathBuf, Opened>,
    clock: u64,
    /// Paths that would not open, and why: reported once each.
    failed: HashMap<PathBuf, String>,
}

/// How many databases the writer keeps open at once. A run names one;
/// tests name a temp dir each.
const OPEN_CAP: usize = 8;

impl Worker {
    fn opened(&mut self, path: &Path) -> Option<&mut Opened> {
        // A test removes its temp dir with the database still open: the
        // next test to reuse the name must not write into the unlinked file.
        if self.open.contains_key(path) && !path.exists() {
            self.open.remove(path);
        }
        if !self.open.contains_key(path) {
            match open_at(path) {
                Ok(conn) => {
                    if self.open.len() >= OPEN_CAP {
                        if let Some(oldest) =
                            self.open.iter().min_by_key(|(_, opened)| opened.last_used).map(|(path, _)| path.clone())
                        {
                            self.open.remove(&oldest);
                        }
                    }
                    self.failed.remove(path);
                    self.open.insert(
                        path.to_owned(),
                        Opened { conn, front: sessions::FrontState::default(), last_used: 0 },
                    );
                }
                Err(error) => {
                    let error = error.to_string();
                    if self.failed.get(path) != Some(&error) {
                        crate::baaz_log!("db: {} does not open ({error}); nothing is read or written", path.display());
                        crate::store::report(crate::store::StoreProblem::DatabaseUnavailable { error: error.clone() });
                        self.failed.insert(path.to_owned(), error);
                    }
                    return None;
                }
            }
        }
        self.clock += 1;
        let clock = self.clock;
        let opened = self.open.get_mut(path)?;
        opened.last_used = clock;
        if clock % CHECKPOINT_EVERY == 0 {
            let _ = opened.conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        }
        Some(opened)
    }
}

type Job = Box<dyn FnOnce(&mut Worker) + Send>;

/// The writer thread's queue, started on first use.
fn queue() -> &'static Mutex<mpsc::Sender<Job>> {
    static QUEUE: OnceLock<Mutex<mpsc::Sender<Job>>> = OnceLock::new();
    QUEUE.get_or_init(|| {
        let (jobs, inbox) = mpsc::channel::<Job>();
        let spawned = std::thread::Builder::new().name("baaz.db".into()).spawn(move || {
            DB_THREAD.with(|on| on.set(true));
            let mut worker = Worker::default();
            while let Ok(job) = inbox.recv() {
                IN_JOB.with(|on| on.set(true));
                // A job that panics rolls its transaction back as it
                // unwinds; the writer carries on with the next one.
                let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&mut worker)));
                IN_JOB.with(|on| on.set(false));
                if ran.is_err() {
                    crate::baaz_log!("db: a job panicked; its writes were rolled back");
                }
            }
        });
        if let Err(error) = spawned {
            crate::baaz_log!("db: no writer thread ({error}); nothing is read or written");
        }
        Mutex::new(jobs)
    })
}

fn submit(job: Job) {
    let jobs = queue().lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
    let _ = jobs.send(job);
}

/// Queue `work` against the database at `path`; returns at once. Work for
/// a database that will not open is dropped (and reported once).
pub fn run(path: PathBuf, work: impl FnOnce(&mut Opened) + Send + 'static) {
    submit(Box::new(move |worker| {
        if let Some(opened) = worker.opened(&path) {
            work(opened);
        }
    }));
}

/// Queue `work` against the database at `path` and wait for its answer:
/// `None` when the database will not open. Never from the writer thread
/// itself (a job that waited on the queue it runs on would never return).
pub fn call<R: Send + 'static>(path: PathBuf, work: impl FnOnce(&mut Opened) -> R + Send + 'static) -> Option<R> {
    assert!(!DB_THREAD.with(Cell::get), "db::call from a db job would wait on itself");
    let (answer, answered) = mpsc::channel();
    submit(Box::new(move |worker| {
        let result = worker.opened(&path).map(work);
        let _ = answer.send(result);
    }));
    answered.recv().ok().flatten()
}

/// [`call`] for a read: a database that is not there yet, with no JSON
/// store beside it to import, answers `None` and is not created by the
/// read (a scripted run or a test that only looks leaves no file behind).
/// Checked on the writer, after every job queued before it.
pub fn read<R: Send + 'static>(path: PathBuf, work: impl FnOnce(&mut Opened) -> R + Send + 'static) -> Option<R> {
    assert!(!DB_THREAD.with(Cell::get), "db::read from a db job would wait on itself");
    let (answer, answered) = mpsc::channel();
    submit(Box::new(move |worker| {
        let there = worker.open.contains_key(&path)
            || path.exists()
            || path.parent().is_some_and(json_import::has_stores);
        let result = if there { worker.opened(&path).map(work) } else { None };
        let _ = answer.send(result);
    }));
    answered.recv().ok().flatten()
}

/// A clean quit (docs/85 11.4): every queued job has run, so the open
/// marker beside this run's database goes.
pub fn mark_clean_exit() {
    flush();
    integrity::mark_clean_exit(&path());
}

/// Wait until every job queued before this one has run: the quit and
/// window-close durability point, and what a test reads back after.
pub fn flush() {
    if DB_THREAD.with(Cell::get) {
        return;
    }
    let (done, finished) = mpsc::channel();
    submit(Box::new(move |_| {
        let _ = done.send(());
    }));
    let _ = finished.recv();
}

/// Drop the writer's connection to `path` (its file is about to go: a
/// test's temp dir). Waits for the jobs queued before it.
pub fn close(path: PathBuf) {
    let (done, finished) = mpsc::channel();
    submit(Box::new(move |worker| {
        worker.open.remove(&path);
        let _ = done.send(());
    }));
    let _ = finished.recv();
}

/// Assert the caller is the writer thread: every connection this module
/// opens is used there and nowhere else.
pub(crate) fn check_writer(what: &str) {
    check_off_ui(what);
    if cfg!(any(test, debug_assertions)) {
        // `open_memory` (unit tests) runs on the test's own thread.
        let allowed = DB_THREAD.with(Cell::get) || MEMORY.with(Cell::get);
        assert!(allowed, "baaz.db: `{what}` ran off the writer thread");
    }
}

thread_local! {
    /// A unit test's in-memory database is used on the test's thread.
    static MEMORY: Cell<bool> = const { Cell::new(false) };
}

/// How long a statement waits for another process to let go of the file.
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);

/// Open the database at `path`: WAL, foreign keys, every pending schema
/// step, then the one-time JSON import. Runs on the writer thread (and in
/// tests that drive a file directly). A newer schema than this build knows,
/// or an import that failed, refuses the open: nothing is read or written
/// for that run, and the file and the JSON stay as they were.
pub fn open_at(path: &Path) -> rusqlite::Result<Connection> {
    check_writer("open");
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    // docs/85 11.4: after an unclean exit the file is checked, and a
    // damaged one comes back from its newest good backup. An in-memory
    // database (`:memory:`) has no file to keep whole: no marker, no
    // backups beside it.
    let on_disk = path.as_os_str() != ":memory:" && !path.as_os_str().is_empty();
    if on_disk && integrity::last_exit_unclean(path) {
        integrity::repair_after_unclean_exit(path);
    }
    let fresh_file = !path.exists();
    let mut conn = Connection::open(path)?;
    let _ = conn.busy_timeout(BUSY_TIMEOUT);
    let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    // docs/85 10.8: WAL makes NORMAL durable across a crash of the app (only
    // a power loss can drop the last commits); temporary b-trees stay in
    // memory; the statement cache holds every statement the jobs reuse.
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = NORMAL; PRAGMA temp_store = MEMORY;")?;
    conn.set_prepared_statement_cache_capacity(64);
    // A migration ahead: room on the disk, and a copy first (11.4).
    let version: i32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if on_disk && !fresh_file && (1..schema::VERSION).contains(&version) {
        if let Err(why) = integrity::before_migration(path, version, integrity::free_bytes(path)) {
            return Err(rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL), Some(why)));
        }
    }
    schema::migrate(&mut conn)?;
    if let Some(state_dir) = path.parent() {
        json_import::run_once(&mut conn, state_dir)?;
        // docs/85 11.2: the old list and index cache files, once.
        if schema::meta(&conn, "list_files_imported")?.is_none() {
            listing::import_old_files(&mut conn, state_dir)?;
            schema::set_meta(&conn, "list_files_imported", "1")?;
        }
    }
    if on_disk {
        integrity::refresh_rolling_backup(&conn, path);
        integrity::mark_open(path);
    }
    Ok(conn)
}

/// A fresh in-memory database at the current schema, used on the calling
/// thread: unit tests of the queries.
#[cfg(any(test, feature = "test-support"))]
pub fn open_memory() -> Connection {
    MEMORY.with(|on| on.set(true));
    let mut conn = Connection::open_in_memory().expect("an in-memory database opens");
    conn.execute_batch("PRAGMA foreign_keys = ON;").expect("foreign keys");
    schema::migrate(&mut conn).expect("the schema builds");
    conn
}

/// Run `work` on the calling thread as if it were the writer: the tests
/// that drive a file directly with [`open_at`].
#[cfg(any(test, feature = "test-support"))]
pub fn as_writer<R>(work: impl FnOnce() -> R) -> R {
    MEMORY.with(|on| on.set(true));
    work()
}

/// Unix milliseconds now.
pub(crate) fn now_ms() -> i64 {
    crate::usage::now_ms()
}

#[cfg(test)]
mod tests;
