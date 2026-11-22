//! `browser.db` as a service (task 4.5, audit F13): history, page words,
//! bookmarks, the never-remember list and each site's settings (its
//! stored permission answers and zoom, docs/82 8.6), read and written off
//! the UI thread.
//!
//! [`BrowserData`] is an entity with two halves:
//!
//! - **The front**, in memory on the UI thread: every bookmark and folder,
//!   the excluded sites, how many pages history holds, and the Field's last
//!   history search, the site settings and the History page's last rows.
//!   Everything the UI reads synchronously (the ★, the Field's bookmark
//!   rows and scoped view, the bookmark popover, Settings › Browser › Your
//!   data and Site permissions, a permission ask's stored answer, a page's
//!   zoom on load) reads this, never the file.
//! - **The worker**, one task on the background executor that owns the one
//!   [`BrowserDb`] connection. Every write and every history search is an
//!   [`Op`] queued to it, run in order; what it learns comes back as a
//!   [`Landed`] on one results channel, drained in order on the UI thread,
//!   and each landing notifies.
//!
//! A write updates the front at once and persists in the background. A new
//! bookmark or folder gets a provisional (negative) id at once; the worker
//! maps it to the row id SQLite assigns and both sides alias the two, so an
//! edit sent before the real id landed still reaches the right row. A
//! snapshot (the first load, and after a Chrome import or its Undo, which
//! only the worker can apply) replaces the front and replays the front's
//! own writes queued after it.
//!
//! Only this module, `history.rs`, `bookmarks.rs` and `chrome_import.rs`
//! name SQLite (`tests/browser_data_ratchet.rs`), and every `BrowserDb`
//! call asserts it runs inside a worker job whenever the UI thread is
//! known ([`check_off_ui`]).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

use futures::channel::{mpsc, oneshot};
use futures::StreamExt as _;
use gpui::{Context, Task};

use super::bookmarks::{self, Bookmark, Folder};
use super::chrome_import::{self, Imported};
use super::history::{self, BrowserDb, HistorySink, Visit};
use super::toolbar::{BookmarkHit, HistoryHit};

use baaz_browser_core::history::{UI_THREAD, IN_JOB};

/// One write or read for the worker, in the order the UI issued them.
pub(crate) enum Op {
    /// Open the file and send the first snapshot (dispatch `seq`).
    Load { seq: u64 },
    Visit { url: String, title: String, now: u64 },
    Title { url: String, title: String },
    Text { url: String, title: String, text: String },
    Exclude(String),
    Include(String),
    ClearHistory,
    SearchHistory { query: String },
    /// The History page's rows for `query` (empty: the newest).
    HistoryPage { query: String },
    /// Forget one page (the History page's delete).
    RemoveVisit(String),
    /// Store (`Some`) or clear (`None`) one site setting.
    Site { origin: String, key: String, value: Option<String> },
    /// Forget everything stored for a site.
    ClearSite(String),
    AddBookmark { provisional: i64, url: String, title: String, folder: i64, now: u64 },
    // Issued only by the probe's `bookmark:` step.
    #[cfg_attr(not(feature = "probe"), allow(dead_code))]
    AddFolder { provisional: i64, name: String, parent: Option<i64>, source: String },
    UpdateBookmark { id: i64, title: String, folder: i64 },
    RemoveBookmark { id: i64 },
    /// A Chrome import, then a snapshot (dispatch `seq`).
    ImportChrome { seq: u64, profile: PathBuf, bookmarks: bool, history: bool, now: u64, reply: oneshot::Sender<Imported> },
    /// Undo one import, then a snapshot (dispatch `seq`).
    UndoImport { seq: u64, source: String },
    /// Every op queued before this one has run: say so (the quit drain,
    /// [`BrowserData::flush_soon`]).
    Flush(std::sync::mpsc::Sender<()>),
}

/// How many pages the History page lists at most (newest first, or the
/// newest matches): a page of history, not the whole file.
const HISTORY_PAGE_ROWS: usize = 500;

/// What the worker sends back, in order.
pub(crate) enum Landed {
    Snapshot { seq: u64, front: Front },
    /// How many pages history holds after a write that may change it.
    Len(u64),
    Hits { query: String, visits: Vec<Visit> },
    /// The History page's rows for `query`.
    Page { query: String, visits: Vec<Visit> },
    /// A provisional bookmark or folder id got its row id.
    Assigned { provisional: i64, real: i64 },
}

/// The history writes the [`history::Recorder`] makes, queued to the worker.
#[derive(Clone)]
pub(crate) struct Writer(mpsc::UnboundedSender<Op>);

impl HistorySink for Writer {
    fn record_visit(&self, url: &str, title: &str, now: u64) {
        let _ = self.0.unbounded_send(Op::Visit { url: url.to_owned(), title: title.to_owned(), now });
    }

    fn set_title(&self, url: &str, title: &str) {
        let _ = self.0.unbounded_send(Op::Title { url: url.to_owned(), title: title.to_owned() });
    }
}

pub(crate) use baaz_browser_core::bookmarks::{FolderRow, Mark};

/// What the UI reads synchronously.
#[derive(Clone, Debug, Default)]
pub(crate) struct Front {
    /// In row order (insertion order, which is SQLite's rowid order).
    pub marks: Vec<Mark>,
    pub folders: Vec<FolderRow>,
    pub excluded: Vec<String>,
    pub history_len: u64,
    /// Each site's stored permission answers and zoom.
    pub sites: baaz_browser_core::sites::SiteSettings,
}

// Issued only by the probe's `bookmark:` step.
#[cfg_attr(not(feature = "probe"), allow(dead_code))]
/// A front write, kept while a snapshot is in flight so it can be replayed
/// over it.
#[derive(Clone, Debug)]
enum FrontOp {
    AddBookmark(Bookmark),
    AddFolder { id: i64, name: String, parent: Option<i64> },
    UpdateBookmark { id: i64, title: String, folder: i64 },
    RemoveBookmark(i64),
    Exclude(String),
    Include(String),
    ClearHistory,
    Site { origin: String, key: String, value: Option<String> },
    ClearSite(String),
    /// One page left history: one fewer until the worker's count lands.
    RemoveVisit,
}

impl Front {
    fn apply(&mut self, op: &FrontOp) {
        match op {
            FrontOp::AddBookmark(mark) => {
                let position = self.marks.iter().filter(|m| m.mark.folder == mark.folder).map(|m| m.position).max().unwrap_or(-1) + 1;
                self.marks.push(Mark { mark: mark.clone(), position });
            }
            FrontOp::AddFolder { id, name, parent } => {
                let position = self.folders.iter().filter(|f| f.parent == *parent).map(|f| f.position).max().unwrap_or(-1) + 1;
                self.folders.push(FolderRow { id: *id, name: name.clone(), parent: *parent, position });
            }
            FrontOp::UpdateBookmark { id, title, folder } => {
                if let Some(m) = self.marks.iter_mut().find(|m| m.mark.id == *id) {
                    m.mark.title = title.clone();
                    m.mark.folder = *folder;
                }
            }
            FrontOp::RemoveBookmark(id) => self.marks.retain(|m| m.mark.id != *id),
            FrontOp::Exclude(site) => {
                if !self.excluded.contains(site) {
                    self.excluded.push(site.clone());
                    self.excluded.sort();
                }
            }
            FrontOp::Include(site) => self.excluded.retain(|s| s != site),
            FrontOp::ClearHistory => self.history_len = 0,
            FrontOp::Site { origin, key, value } => self.sites.set(origin, key, value.as_deref()),
            FrontOp::ClearSite(origin) => self.sites.clear_site(origin),
            FrontOp::RemoveVisit => self.history_len = self.history_len.saturating_sub(1),
        }
    }

    /// `id`'s row ids rewritten to `real` everywhere (a provisional id
    /// landed). A bookmark the worker found already there keeps one row.
    fn rename(&mut self, provisional: i64, real: i64) {
        if self.marks.iter().any(|m| m.mark.id == real) {
            self.marks.retain(|m| m.mark.id != provisional);
        }
        for m in &mut self.marks {
            if m.mark.id == provisional {
                m.mark.id = real;
            }
            if m.mark.folder == provisional {
                m.mark.folder = real;
            }
        }
        for f in &mut self.folders {
            if f.id == provisional {
                f.id = real;
            }
            if f.parent == Some(provisional) {
                f.parent = Some(real);
            }
        }
    }
}

impl FrontOp {
    fn rename(&mut self, provisional: i64, real: i64) {
        let fix = |id: &mut i64| {
            if *id == provisional {
                *id = real;
            }
        };
        match self {
            FrontOp::AddBookmark(mark) => {
                fix(&mut mark.id);
                fix(&mut mark.folder);
            }
            FrontOp::AddFolder { id, parent, .. } => {
                fix(id);
                if let Some(parent) = parent.as_mut() {
                    fix(parent);
                }
            }
            FrontOp::UpdateBookmark { id, folder, .. } => {
                fix(id);
                fix(folder);
            }
            FrontOp::RemoveBookmark(id) => fix(id),
            FrontOp::Exclude(_) | FrontOp::Include(_) | FrontOp::ClearHistory | FrontOp::Site { .. } | FrontOp::ClearSite(_) | FrontOp::RemoveVisit => {}
        }
    }
}

/// The Field's last history search.
struct Hits {
    query: String,
    visits: Vec<Visit>,
    /// False once history changed since: the next read asks again and
    /// shows these until the fresh rows land.
    fresh: bool,
}

/// The `browser.db` service (see the module doc).
pub(crate) struct BrowserData {
    jobs: mpsc::UnboundedSender<Op>,
    front: Front,
    /// Front writes issued so far (the journal's clock).
    seq: u64,
    /// Snapshots asked for and not landed yet, by dispatch `seq`.
    snapshots_out: Vec<u64>,
    /// Front writes issued after the oldest outstanding snapshot.
    journal: Vec<(u64, FrontOp)>,
    next_provisional: i64,
    /// Provisional id → row id, for ids the UI still holds.
    alias: HashMap<i64, i64>,
    hits: RefCell<Option<Hits>>,
    /// The search in flight, so a render never asks twice.
    asked: RefCell<Option<String>>,
    /// The History page's last rows, kept like the Field's hits.
    page: RefCell<Option<Hits>>,
    /// The History page query in flight.
    page_asked: RefCell<Option<String>>,
    _worker: Task<()>,
    _drain: Task<()>,
}

impl BrowserData {
    /// The owner's store (in memory under captures and tests, see
    /// [`BrowserDb::open`]), opened by the worker.
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        Self::with_opener(BrowserDb::open, cx)
    }

    /// A store at `path` (tests: a temp state dir).
    #[cfg(all(test, feature = "probe"))]
    pub(crate) fn at(path: PathBuf, cx: &mut Context<Self>) -> Self {
        Self::with_opener(move || BrowserDb::open_at(&path).expect("the test browser.db opens"), cx)
    }

    fn with_opener(open: impl FnOnce() -> BrowserDb + Send + 'static, cx: &mut Context<Self>) -> Self {
        UI_THREAD.with(|ui| ui.set(true));
        let (jobs, mut queue) = mpsc::unbounded::<Op>();
        let (results, mut landed) = mpsc::unbounded::<Landed>();
        let _worker = cx.background_executor().spawn(async move {
            let mut worker = Worker { open: Some(Box::new(open)), db: None, alias: HashMap::new() };
            while let Some(op) = queue.next().await {
                IN_JOB.with(|job| job.set(true));
                worker.run(op, &results);
                IN_JOB.with(|job| job.set(false));
            }
        });
        let _drain = cx.spawn(async move |this, cx| {
            while let Some(landed) = landed.next().await {
                if this.update(cx, |data, cx| data.land(landed, cx)).is_err() {
                    break;
                }
            }
        });
        let mut data = Self {
            jobs,
            front: Front::default(),
            seq: 0,
            snapshots_out: Vec::new(),
            journal: Vec::new(),
            next_provisional: -1,
            alias: HashMap::new(),
            hits: RefCell::new(None),
            asked: RefCell::new(None),
            page: RefCell::new(None),
            page_asked: RefCell::new(None),
            _worker,
            _drain,
        };
        let seq = data.snapshot_seq();
        data.send(Op::Load { seq });
        data
    }

    fn send(&self, op: Op) {
        let _ = self.jobs.unbounded_send(op);
    }

    /// How long quit waits for the worker to finish the writes queued
    /// before it: long enough for a history write and a page's words, short
    /// enough that a stuck file never holds the quit.
    pub(crate) const QUIT_DRAIN: std::time::Duration = std::time::Duration::from_millis(500);

    /// Ask the worker to say when every write queued so far has reached
    /// the file: the receiver hears once it has (ops run in order).
    pub(crate) fn flush_soon(&self) -> std::sync::mpsc::Receiver<()> {
        let (done, heard) = std::sync::mpsc::channel();
        self.send(Op::Flush(done));
        heard
    }

    /// Quit: block (at most `bound`) until every write queued so far has
    /// reached the file, so a visit or a bookmark made just before ⌘Q is
    /// not lost with the worker. Answers whether the worker caught up in
    /// time. The worker runs on the background executor's threads, so the
    /// wait never starves it.
    pub(crate) fn drain_at_quit(&self, bound: std::time::Duration) -> bool {
        let caught_up = self.flush_soon().recv_timeout(bound).is_ok();
        if !caught_up {
            crate::baaz_log!("browser.db: quit drain gave up after {} ms", bound.as_millis());
        }
        caught_up
    }

    /// A snapshot is asked for now: front writes from here on are journaled.
    fn snapshot_seq(&mut self) -> u64 {
        self.snapshots_out.push(self.seq);
        self.seq
    }

    /// Apply `op` to the front at once (the worker gets it separately) and
    /// redraw: every bookmark, folder and history edit comes through here.
    fn apply(&mut self, op: FrontOp, cx: &mut Context<Self>) {
        self.seq += 1;
        self.front.apply(&op);
        if !self.snapshots_out.is_empty() {
            self.journal.push((self.seq, op));
        }
        cx.notify();
    }

    fn provisional(&mut self) -> i64 {
        let id = self.next_provisional;
        self.next_provisional -= 1;
        id
    }

    /// The row id behind an id the UI holds (a provisional one that landed).
    fn resolve(&self, id: i64) -> i64 {
        self.alias.get(&id).copied().unwrap_or(id)
    }

    fn land(&mut self, landed: Landed, cx: &mut Context<Self>) {
        match landed {
            Landed::Snapshot { seq, front } => {
                self.snapshots_out.retain(|out| *out != seq);
                self.front = front;
                for (_, op) in self.journal.iter().filter(|(at, _)| *at > seq) {
                    self.front.apply(op);
                }
                let oldest = self.snapshots_out.iter().copied().min();
                match oldest {
                    Some(oldest) => self.journal.retain(|(at, _)| *at > oldest),
                    None => self.journal.clear(),
                }
                self.history_changed();
            }
            Landed::Len(len) => {
                if self.front.history_len == len {
                    return;
                }
                self.front.history_len = len;
            }
            Landed::Hits { query, visits } => {
                if self.asked.borrow().as_deref() == Some(query.as_str()) {
                    self.asked.borrow_mut().take();
                }
                let same = self.hits.borrow().as_ref().is_some_and(|hits| hits.query == query && hits.visits == visits);
                *self.hits.borrow_mut() = Some(Hits { query, visits, fresh: true });
                if same {
                    // Nothing on screen changes: no frame (a re-ask after
                    // history changed must not keep the window awake).
                    return;
                }
            }
            Landed::Page { query, visits } => {
                if self.page_asked.borrow().as_deref() == Some(query.as_str()) {
                    self.page_asked.borrow_mut().take();
                }
                let same = self.page.borrow().as_ref().is_some_and(|page| page.query == query && page.visits == visits);
                *self.page.borrow_mut() = Some(Hits { query, visits, fresh: true });
                if same {
                    return;
                }
            }
            Landed::Assigned { provisional, real } => {
                self.alias.insert(provisional, real);
                self.front.rename(provisional, real);
                for (_, op) in &mut self.journal {
                    op.rename(provisional, real);
                }
            }
        }
        cx.notify();
    }

    // ---- reads (the front) ----

    /// The person's bookmark for `url`, if any (the newest when several).
    pub(crate) fn bookmark(&self, url: &str) -> Option<Bookmark> {
        self.front
            .marks
            .iter()
            .enumerate()
            .filter(|(_, m)| m.mark.url == url)
            .max_by_key(|(at, m)| (m.mark.added_ms, *at))
            .map(|(_, m)| m.mark.clone())
    }

    /// Whether `url` is bookmarked (the ★).
    pub(crate) fn is_bookmarked(&self, url: &str) -> bool {
        history::recordable(url) && self.front.marks.iter().any(|m| m.mark.url == url)
    }

    /// One bookmark by id.
    pub(crate) fn bookmark_by_id(&self, id: i64) -> Option<Bookmark> {
        let id = self.resolve(id);
        self.front.marks.iter().find(|m| m.mark.id == id).map(|m| m.mark.clone())
    }

    /// Every folder, in tree order (parents before children, by position),
    /// with how many bookmarks and folders sit directly in it.
    pub(crate) fn folders(&self) -> Vec<Folder> {
        let mut rows: Vec<&FolderRow> = self.front.folders.iter().collect();
        rows.sort_by_key(|f| (f.parent.unwrap_or(0), f.position, f.id));
        rows.into_iter()
            .map(|f| Folder {
                id: f.id,
                name: f.name.clone(),
                parent: f.parent,
                count: (self.front.marks.iter().filter(|m| m.mark.folder == f.id).count()
                    + self.front.folders.iter().filter(|c| c.parent == Some(f.id)).count()) as u64,
            })
            .collect()
    }

    /// Bookmarks whose title or address contains `query` (ASCII case
    /// folded, like SQLite's `LIKE`), newest first.
    pub(crate) fn search_bookmarks(&self, query: &str, limit: usize) -> Vec<Bookmark> {
        let q = query.trim().to_ascii_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<&Mark> = self
            .front
            .marks
            .iter()
            .filter(|m| m.mark.title.to_ascii_lowercase().contains(&q) || m.mark.url.to_ascii_lowercase().contains(&q))
            .collect();
        hits.sort_by_key(|m| std::cmp::Reverse(m.mark.added_ms));
        hits.into_iter().take(limit).map(|m| m.mark.clone()).collect()
    }

    /// The bookmarks directly in `folder`, by position.
    pub(crate) fn in_folder(&self, folder: i64) -> Vec<Bookmark> {
        let folder = self.resolve(folder);
        let mut rows: Vec<(usize, &Mark)> = self.front.marks.iter().enumerate().filter(|(_, m)| m.mark.folder == folder).collect();
        rows.sort_by_key(|(at, m)| (m.position, *at));
        rows.into_iter().map(|(_, m)| m.mark.clone()).collect()
    }

    /// The Field's bookmark rows for `query`.
    pub(crate) fn bookmark_hits(&self, query: &str) -> Vec<BookmarkHit> {
        let all = self.folders();
        self.search_bookmarks(query, 12)
            .into_iter()
            .map(|mark| BookmarkHit { folder: bookmarks::folder_path(&all, mark.folder), title: mark.title, url: mark.url })
            .collect()
    }

    /// The Field's history rows for `query`: the last search's rows for it,
    /// asking the worker when there are none (or history changed since),
    /// and meanwhile the last rows shown, so the list never blinks empty.
    pub(crate) fn history_hits(&self, query: &str) -> Vec<HistoryHit> {
        let query = query.trim();
        if query.is_empty() {
            return Vec::new();
        }
        let current = self.hits.borrow().as_ref().is_some_and(|hits| hits.fresh && hits.query == query);
        if !current && self.asked.borrow().as_deref() != Some(query) {
            *self.asked.borrow_mut() = Some(query.to_owned());
            self.send(Op::SearchHistory { query: query.to_owned() });
        }
        let now = super::tabs::now_ms();
        self.hits
            .borrow()
            .as_ref()
            .map(|hits| {
                hits.visits
                    .iter()
                    .map(|visit| HistoryHit {
                        ago: history::ago(visit.last_ms, now),
                        title: visit.title.clone(),
                        url: visit.url.clone(),
                        last_ms: visit.last_ms,
                        snippet: visit.snippet.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// How many pages history holds.
    pub(crate) fn history_len(&self) -> u64 {
        self.front.history_len
    }

    /// The sites history never records, sorted.
    pub(crate) fn excluded(&self) -> &[String] {
        &self.front.excluded
    }

    #[cfg(feature = "probe")]
    /// Whether `url`'s site is on the never-remember list.
    pub(crate) fn is_excluded(&self, url: &str) -> bool {
        let host = history::host_of(url);
        self.front.excluded.iter().any(|site| host == *site || host.ends_with(&format!(".{site}")))
    }

    /// History changed: the Field's rows and the History page's are asked
    /// for again on the next read (and shown as they are until then).
    pub(crate) fn history_changed(&self) {
        if let Some(hits) = self.hits.borrow_mut().as_mut() {
            hits.fresh = false;
        }
        self.asked.borrow_mut().take();
        if let Some(page) = self.page.borrow_mut().as_mut() {
            page.fresh = false;
        }
        self.page_asked.borrow_mut().take();
    }

    /// The History page's rows for `query` (empty: the newest pages): the
    /// last rows read for it, asking the worker when there are none (or
    /// history changed since) and meanwhile the last rows shown. `None`
    /// until the first rows land.
    pub(crate) fn history_page(&self, query: &str) -> Option<Vec<Visit>> {
        let query = query.trim();
        let current = self.page.borrow().as_ref().is_some_and(|page| page.fresh && page.query == query);
        if !current && self.page_asked.borrow().as_deref() != Some(query) {
            *self.page_asked.borrow_mut() = Some(query.to_owned());
            self.send(Op::HistoryPage { query: query.to_owned() });
        }
        self.page.borrow().as_ref().map(|page| page.visits.clone())
    }

    /// Forget one page (the History page's delete): gone from the page's
    /// rows at once, from the file in the background.
    pub(crate) fn remove_visit(&mut self, url: &str, cx: &mut Context<Self>) {
        if let Some(page) = self.page.borrow_mut().as_mut() {
            page.visits.retain(|visit| visit.url != url);
        }
        if let Some(hits) = self.hits.borrow_mut().as_mut() {
            hits.visits.retain(|visit| visit.url != url);
        }
        self.send(Op::RemoveVisit(url.to_owned()));
        self.history_changed();
        self.apply(FrontOp::RemoveVisit, cx);
    }

    /// Every site's stored answers and zoom.
    pub(crate) fn sites(&self) -> &baaz_browser_core::sites::SiteSettings {
        &self.front.sites
    }

    /// Store (`Some`) or clear (`None`) one setting for `origin`: the front
    /// at once, the file in the background.
    pub(crate) fn set_site(&mut self, origin: &str, key: &str, value: Option<&str>, cx: &mut Context<Self>) {
        if self.front.sites.origins().iter().any(|o| o == origin) || value.is_some() {
            let (origin, key, value) = (origin.to_owned(), key.to_owned(), value.map(str::to_owned));
            self.apply(FrontOp::Site { origin: origin.clone(), key: key.clone(), value: value.clone() }, cx);
            self.send(Op::Site { origin, key, value });
        }
    }

    /// Forget everything stored for `origin`.
    pub(crate) fn clear_site(&mut self, origin: &str, cx: &mut Context<Self>) {
        self.apply(FrontOp::ClearSite(origin.to_owned()), cx);
        self.send(Op::ClearSite(origin.to_owned()));
    }

    /// The writer the history recorder queues visits and titles through.
    pub(crate) fn writer(&self) -> Writer {
        Writer(self.jobs.clone())
    }

    // ---- writes (front at once, file in the background) ----

    /// Keep a page's words for search (after the dwell).
    pub(crate) fn set_text(&mut self, url: &str, title: &str, text: &str) {
        self.send(Op::Text { url: url.to_owned(), title: title.to_owned(), text: text.to_owned() });
        self.history_changed();
    }

    #[cfg(feature = "probe")]
    /// A visit recorded outside the recorder (the `history:` step).
    pub(crate) fn record_visit(&mut self, url: &str, title: &str, now: u64) {
        self.writer().record_visit(url, title, now);
        self.history_changed();
    }

    /// Never record `site` again, and forget what is kept of it.
    pub(crate) fn exclude(&mut self, site: &str, cx: &mut Context<Self>) {
        let site = history::host_of(site);
        if site.is_empty() {
            return;
        }
        self.apply(FrontOp::Exclude(site.clone()), cx);
        // The Field stops showing the site's pages now, not when the next
        // search lands.
        if let Some(hits) = self.hits.borrow_mut().as_mut() {
            hits.visits.retain(|visit| {
                let host = history::host_of(&visit.url);
                host != site && !host.ends_with(&format!(".{site}"))
            });
        }
        self.send(Op::Exclude(site));
        self.history_changed();
    }

    /// Record `site` again.
    pub(crate) fn include(&mut self, site: &str, cx: &mut Context<Self>) {
        let site = history::host_of(site);
        self.apply(FrontOp::Include(site.clone()), cx);
        self.send(Op::Include(site));
    }

    /// Forget all history.
    pub(crate) fn clear_history(&mut self, cx: &mut Context<Self>) {
        self.apply(FrontOp::ClearHistory, cx);
        self.send(Op::ClearHistory);
        *self.hits.borrow_mut() = None;
        self.asked.borrow_mut().take();
        *self.page.borrow_mut() = Some(Hits { query: String::new(), visits: Vec::new(), fresh: true });
        self.page_asked.borrow_mut().take();
    }

    /// Bookmark `url` into `folder` (⌘D: the Bookmarks bar): the existing
    /// bookmark when the page has one, else a new one with a provisional id.
    pub(crate) fn add_bookmark(&mut self, url: &str, title: &str, folder: i64, now: u64, cx: &mut Context<Self>) -> Bookmark {
        if let Some(existing) = self.bookmark(url) {
            return existing;
        }
        let folder = self.resolve(folder);
        let id = self.provisional();
        let mark = Bookmark { id, url: url.to_owned(), title: title.to_owned(), folder, added_ms: now };
        self.apply(FrontOp::AddBookmark(mark.clone()), cx);
        self.send(Op::AddBookmark { provisional: id, url: url.to_owned(), title: title.to_owned(), folder, now });
        mark
    }

    #[cfg(feature = "probe")]
    /// Make a folder (the person's own): its provisional id.
    pub(crate) fn add_folder(&mut self, name: &str, parent: Option<i64>, cx: &mut Context<Self>) -> i64 {
        let parent = parent.map(|p| self.resolve(p));
        let id = self.provisional();
        self.apply(FrontOp::AddFolder { id, name: name.to_owned(), parent }, cx);
        self.send(Op::AddFolder { provisional: id, name: name.to_owned(), parent, source: "baaz".to_owned() });
        id
    }

    /// Rename a bookmark and move it to `folder`.
    pub(crate) fn update_bookmark(&mut self, id: i64, title: &str, folder: i64, cx: &mut Context<Self>) {
        let (id, folder) = (self.resolve(id), self.resolve(folder));
        self.apply(FrontOp::UpdateBookmark { id, title: title.to_owned(), folder }, cx);
        self.send(Op::UpdateBookmark { id, title: title.to_owned(), folder });
    }

    /// Remove one bookmark.
    pub(crate) fn remove_bookmark(&mut self, id: i64, cx: &mut Context<Self>) {
        let id = self.resolve(id);
        self.apply(FrontOp::RemoveBookmark(id), cx);
        self.send(Op::RemoveBookmark { id });
    }

    /// Import a Chrome profile in the worker; the answer says what it wrote,
    /// and a snapshot follows it into the front.
    pub(crate) fn import_chrome(&mut self, profile: PathBuf, bookmarks: bool, history: bool, now: u64) -> oneshot::Receiver<Imported> {
        let (reply, answer) = oneshot::channel();
        let seq = self.snapshot_seq();
        self.send(Op::ImportChrome { seq, profile, bookmarks, history, now, reply });
        answer
    }

    /// Undo one import in the worker; a snapshot follows.
    pub(crate) fn undo_import(&mut self, source: &str) {
        let seq = self.snapshot_seq();
        self.send(Op::UndoImport { seq, source: source.to_owned() });
    }
}

/// The background half: the one connection, opened by the first job.
struct Worker {
    open: Option<Box<dyn FnOnce() -> BrowserDb + Send>>,
    db: Option<BrowserDb>,
    /// Provisional id → row id, so a write sent before its id landed on the
    /// UI side reaches the row.
    alias: HashMap<i64, i64>,
}

impl Worker {
    fn db(&mut self) -> &BrowserDb {
        if self.db.is_none() {
            let open = self.open.take().expect("browser.db opens once");
            self.db = Some(open());
        }
        self.db.as_ref().expect("browser.db is open")
    }

    fn resolve(&self, id: i64) -> i64 {
        self.alias.get(&id).copied().unwrap_or(id)
    }

    fn snapshot(&mut self) -> Front {
        let db = self.db();
        let (marks, folders) = bookmarks::load_front(db.conn());
        Front { marks, folders, excluded: db.excluded(), history_len: db.len(), sites: db.site_settings() }
    }

    fn run(&mut self, op: Op, out: &mpsc::UnboundedSender<Landed>) {
        let send = |landed: Landed| {
            let _ = out.unbounded_send(landed);
        };
        match op {
            Op::Load { seq } => send(Landed::Snapshot { seq, front: self.snapshot() }),
            Op::Visit { url, title, now } => {
                if self.db().record_visit(&url, &title, now) {
                    send(Landed::Len(self.db().len()));
                }
            }
            Op::Title { url, title } => self.db().set_title(&url, &title),
            Op::Text { url, title, text } => self.db().set_text(&url, &title, &text),
            Op::Exclude(site) => {
                self.db().exclude(&site);
                send(Landed::Len(self.db().len()));
            }
            Op::Include(site) => self.db().include(&site),
            Op::ClearHistory => {
                self.db().clear_since(0);
                send(Landed::Len(self.db().len()));
            }
            Op::SearchHistory { query } => {
                let visits = self.db().search(&query, 12);
                send(Landed::Hits { query, visits });
            }
            Op::HistoryPage { query } => {
                let visits = self.db().history_page(&query, HISTORY_PAGE_ROWS);
                send(Landed::Page { query, visits });
            }
            Op::RemoveVisit(url) => {
                self.db().remove(&url);
                send(Landed::Len(self.db().len()));
            }
            Op::Site { origin, key, value } => self.db().set_site(&origin, &key, value.as_deref()),
            Op::ClearSite(origin) => self.db().clear_site(&origin),
            Op::AddBookmark { provisional, url, title, folder, now } => {
                let folder = self.resolve(folder);
                if let Some(mark) = bookmarks::add(self.db().conn(), &url, &title, folder, now) {
                    self.alias.insert(provisional, mark.id);
                    send(Landed::Assigned { provisional, real: mark.id });
                }
            }
            Op::AddFolder { provisional, name, parent, source } => {
                let parent = parent.map(|p| self.resolve(p));
                if let Some(id) = bookmarks::add_folder(self.db().conn(), &name, parent, &source) {
                    self.alias.insert(provisional, id);
                    send(Landed::Assigned { provisional, real: id });
                }
            }
            Op::UpdateBookmark { id, title, folder } => {
                let (id, folder) = (self.resolve(id), self.resolve(folder));
                bookmarks::update(self.db().conn(), id, &title, folder);
            }
            Op::RemoveBookmark { id } => {
                let id = self.resolve(id);
                bookmarks::remove(self.db().conn(), id);
            }
            Op::ImportChrome { seq, profile, bookmarks, history, now, reply } => {
                let done = chrome_import::import(self.db(), &profile, bookmarks, history, now, |_, _| {});
                send(Landed::Snapshot { seq, front: self.snapshot() });
                let _ = reply.send(done);
            }
            Op::UndoImport { seq, source } => {
                chrome_import::undo(self.db(), &source);
                send(Landed::Snapshot { seq, front: self.snapshot() });
            }
            Op::Flush(done) => {
                let _ = done.send(());
            }
        }
    }
}

#[cfg(all(test, feature = "probe"))]
mod tests {
    use super::*;
    use gpui::AppContext as _;

    fn temp_db(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("baaz-browser-data-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("browser.db")
    }

    /// Reads the file the way another process would: on its own thread, so
    /// the UI-thread guard never sees it.
    fn on_disk<T: Send + 'static>(path: &std::path::Path, read: impl FnOnce(&BrowserDb) -> T + Send + 'static) -> T {
        let path = path.to_owned();
        std::thread::spawn(move || read(&BrowserDb::open_at(&path).unwrap())).join().unwrap()
    }

    #[gpui::test]
    fn a_bookmark_reads_from_the_front_at_once_and_lands_in_the_file(cx: &mut gpui::TestAppContext) {
        let path = temp_db("bookmark");
        let data = cx.new(|cx| BrowserData::at(path.clone(), cx));
        cx.run_until_parked();
        // Written before anything parks: the ★ and the Field see it in the
        // same frame, with no SQLite call on this thread.
        let mark = data.update(cx, |data, cx| data.add_bookmark("https://rust-lang.org/learn", "Learn Rust", bookmarks::BAR, 5, cx));
        assert!(mark.id < 0, "a provisional id until the row lands");
        data.read_with(cx, |data, _| {
            assert!(data.is_bookmarked("https://rust-lang.org/learn"));
            assert_eq!(data.bookmark_hits("rust").len(), 1);
            assert_eq!(data.in_folder(bookmarks::BAR).len(), 1);
        });
        assert!(on_disk(&path, |db| bookmarks::find(db.conn(), "https://rust-lang.org/learn")).is_none(), "not written yet");
        cx.run_until_parked();
        let real = data.read_with(cx, |data, _| data.bookmark("https://rust-lang.org/learn").unwrap().id);
        assert!(real > 0, "the row id landed");
        // An edit through the provisional id the popover still holds.
        data.update(cx, |data, cx| data.update_bookmark(mark.id, "The Book", bookmarks::OTHER, cx));
        cx.run_until_parked();
        let row = on_disk(&path, |db| bookmarks::find(db.conn(), "https://rust-lang.org/learn")).expect("in the file");
        assert_eq!((row.id, row.title.as_str(), row.folder), (real, "The Book", bookmarks::OTHER));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Task 4.5b: quit drains the worker. A visit and a bookmark queued just
    /// before quit are in the file once the flush is heard: the flush runs
    /// behind every write queued before it, never ahead. The bounded wait
    /// never hangs: on the test executor the worker cannot run while this
    /// thread waits, so the drain gives up at its bound and says so.
    /// docs/82 8.6: a site's kept answer and zoom read from the front at
    /// once, reach the file, and come back with the next launch's store;
    /// the History page's rows come from the worker and a deleted page
    /// leaves them at once.
    #[gpui::test]
    fn site_settings_and_the_history_page_survive_a_relaunch(cx: &mut gpui::TestAppContext) {
        let path = temp_db("site-settings");
        let data = cx.new(|cx| BrowserData::at(path.clone(), cx));
        cx.run_until_parked();
        let now = crate::browser::tabs::now_ms();
        data.update(cx, |data, cx| {
            data.set_site("https://cam.example", "camera", Some("allow"), cx);
            data.set_site("https://cam.example", baaz_browser_core::sites::ZOOM_KEY, Some("150"), cx);
            data.writer().record_visit("https://a.example/", "Alpha", now);
            data.writer().record_visit("https://b.example/", "Beta", now);
            data.history_changed();
        });
        data.read_with(cx, |data, _| {
            assert_eq!(data.sites().answer("https://cam.example", "camera"), Some(baaz_browser_core::sites::Answer::Allow), "the front has it at once");
            assert_eq!(data.history_page(""), None, "the page's rows are asked for, not read here");
        });
        cx.run_until_parked();
        let rows = data.read_with(cx, |data, _| data.history_page("")).expect("the rows landed");
        assert_eq!(rows.len(), 2);
        data.update(cx, |data, cx| data.remove_visit("https://a.example/", cx));
        assert_eq!(data.read_with(cx, |data, _| data.history_page("")).map(|rows| rows.len()), Some(1), "gone from the page at once");
        cx.run_until_parked();
        assert_eq!(on_disk(&path, |db| db.len()), 1, "and from the file");
        drop(data);
        // The next launch: a fresh store on the same file.
        let again = cx.new(|cx| BrowserData::at(path.clone(), cx));
        cx.run_until_parked();
        again.read_with(cx, |data, _| {
            assert_eq!(data.sites().answer("https://cam.example", "camera"), Some(baaz_browser_core::sites::Answer::Allow));
            assert_eq!(data.sites().zoom("https://cam.example"), Some(150));
        });
        again.update(cx, |data, cx| data.clear_site("https://cam.example", cx));
        cx.run_until_parked();
        assert!(on_disk(&path, |db| db.site_settings().origins().is_empty()), "Forget reaches the file");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[gpui::test]
    fn writes_queued_before_quit_are_in_the_file_after_the_drain(cx: &mut gpui::TestAppContext) {
        let path = temp_db("quit-drain");
        let data = cx.new(|cx| BrowserData::at(path.clone(), cx));
        cx.run_until_parked();
        // A real clock: reading the file back opens it, which prunes old visits.
        let now = crate::browser::tabs::now_ms();
        data.read_with(cx, |data, _| data.writer().record_visit("https://late.example/", "Late", now));
        data.update(cx, |data, cx| data.add_bookmark("https://late.example/", "Late", bookmarks::BAR, now, cx));
        let heard = data.read_with(cx, |data, _| data.flush_soon());
        assert!(heard.try_recv().is_err(), "nothing has run yet");
        assert_eq!(on_disk(&path, |db| db.len()), 0, "the visit is still queued");
        cx.run_until_parked();
        assert!(heard.try_recv().is_ok(), "the worker reached the flush");
        assert_eq!(on_disk(&path, |db| db.len()), 1, "the visit queued before it is in the file");
        assert!(on_disk(&path, |db| bookmarks::find(db.conn(), "https://late.example/")).is_some(), "so is the bookmark");
        let started = std::time::Instant::now();
        let caught_up = data.read_with(cx, |data, _| data.drain_at_quit(std::time::Duration::from_millis(50)));
        assert!(!caught_up, "a worker that cannot run is given up on at the bound");
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "the bound holds");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[gpui::test]
    fn writes_before_the_load_lands_survive_it(cx: &mut gpui::TestAppContext) {
        let path = temp_db("journal");
        on_disk(&path, |db| bookmarks::add(db.conn(), "https://old.example/", "Old", bookmarks::BAR, 1));
        let data = cx.new(|cx| BrowserData::at(path.clone(), cx));
        // Nothing parked: the load has not landed, the front is empty.
        let folder = data.update(cx, |data, cx| data.add_folder("Work", None, cx));
        data.update(cx, |data, cx| data.add_bookmark("https://new.example/", "New", folder, 2, cx));
        cx.run_until_parked();
        data.read_with(cx, |data, _| {
            assert!(data.is_bookmarked("https://old.example/"), "the snapshot landed");
            let new = data.bookmark("https://new.example/").expect("the journaled add was replayed over the snapshot");
            let work = data.folders().into_iter().find(|f| f.name == "Work").expect("so was the folder");
            assert!(work.id > 0 && new.folder == work.id, "both carry their row ids");
            assert_eq!(work.count, 1);
        });
        let folder_on_disk = on_disk(&path, |db| bookmarks::find(db.conn(), "https://new.example/").map(|m| m.folder)).unwrap();
        let work = data.read_with(cx, |data, _| data.folders().into_iter().find(|f| f.name == "Work").unwrap().id);
        assert_eq!(folder_on_disk, work, "the worker resolved the provisional folder");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[gpui::test]
    fn history_search_lands_later_and_notifies(cx: &mut gpui::TestAppContext) {
        let path = temp_db("history");
        let data = cx.new(|cx| BrowserData::at(path.clone(), cx));
        let notified = std::rc::Rc::new(std::cell::Cell::new(0));
        let seen = notified.clone();
        let _sub = cx.update(|cx| cx.observe(&data, move |_, _| seen.set(seen.get() + 1)));
        data.update(cx, |data, _| {
            data.record_visit("https://without.boats/blog/pin/", "Pin and suffering", 10);
            data.set_text("https://without.boats/blog/pin/", "Pin and suffering", "Self-referential futures in Rust need Pin");
        });
        assert!(data.read_with(cx, |data, _| data.history_hits("rust")).is_empty(), "asked, not answered yet");
        let before = notified.get();
        cx.run_until_parked();
        assert!(notified.get() > before, "the landing notifies");
        let hits = data.read_with(cx, |data, _| data.history_hits("rust"));
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.as_deref().unwrap_or("").contains("Rust"));
        assert_eq!(data.read_with(cx, |data, _| data.history_len()), 1);
        // Excluding forgets at once on the front and in the file.
        data.update(cx, |data, cx| data.exclude("https://www.without.boats/", cx));
        assert_eq!(data.read_with(cx, |data, _| data.excluded().to_vec()), vec!["without.boats".to_owned()]);
        cx.run_until_parked();
        assert_eq!(data.read_with(cx, |data, _| data.history_len()), 0);
        assert!(data.read_with(cx, |data, _| data.history_hits("rust")).is_empty());
        assert_eq!(on_disk(&path, |db| db.len()), 0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[gpui::test]
    fn the_front_orders_like_the_file(cx: &mut gpui::TestAppContext) {
        let data = cx.new(|cx| BrowserData::at(temp_db("order"), cx));
        cx.run_until_parked();
        data.update(cx, |data, cx| {
            let learning = data.add_folder("Learning", Some(bookmarks::OTHER), cx);
            data.add_bookmark("https://doc.rust-lang.org/book", "The Rust Programming Language", bookmarks::BAR, 1, cx);
            data.add_bookmark("https://doc.rust-lang.org/rust-by-example", "Rust by Example", learning, 2, cx);
            data.add_bookmark("https://news.ycombinator.com/", "Hacker News", bookmarks::OTHER, 3, cx);
            let hits = data.search_bookmarks("RUST", 10);
            assert_eq!(hits.len(), 2);
            assert_eq!(hits[0].title, "Rust by Example", "newest first, ASCII case folded");
            let all = data.folders();
            assert_eq!(bookmarks::folder_path(&all, learning), "Other › Learning");
            assert_eq!(all.iter().find(|f| f.id == bookmarks::OTHER).unwrap().count, 2, "one bookmark and one folder");
            assert!(data.search_bookmarks("%", 10).is_empty(), "% is literal");
        });
        cx.run_until_parked();
        // The same answers once every id is a row id.
        data.read_with(cx, |data, _| {
            assert_eq!(data.search_bookmarks("rust", 10)[0].title, "Rust by Example");
            assert_eq!(data.folders().iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), vec!["Bookmarks bar", "Other", "Learning"]);
        });
    }

    #[gpui::test]
    #[should_panic(expected = "ran on the UI thread outside a worker job")]
    fn sqlite_on_the_ui_thread_outside_the_worker_panics(cx: &mut gpui::TestAppContext) {
        let _data = cx.new(BrowserData::new);
        let db = BrowserDb::memory();
        db.len();
    }
}
