//! P7 import, the surfaces (docs/72 §3, the approved P7Import board).
//!
//! * The offer on each provider's Settings card: one line under Details —
//!   "N Claude Code sessions on this Mac, in M folders. None are in Baaz
//!   yet. [Import…]", after an import "N imported 30 Oct · K left on this
//!   Mac [Import more…]", "95 Codex sessions on this Mac, all scripted runs
//!   (codex exec)" when every one is scripted, nothing when the home has
//!   none.
//! * First run: once a provider is Connected and its home holds sessions
//!   not in Baaz, one line under the connect rows (the screen's assist
//!   line): "Found N earlier sessions on this Mac. Import…".
//! * The import sheet (one for both providers): folders, the scripted-runs
//!   option, the in-place note, Cancel / Import N sessions; then progress
//!   (Close keeps it running) and the result.
//! * The rows land in the sidebar as batches land; the toast after a run
//!   offers Undo, which removes exactly the run's records and never a file.
//!
//! Every count a render reads comes from [`ImportUi::cache`], computed off
//! the UI thread by [`Harness::refresh_import_scan`] and refreshed
//! stale-while-revalidate (like the provider statuses): no render walks a
//! home.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;

use aui::data::{button, icon_button, ButtonSize};
use aui::skills::{checkbox, CheckboxIntent, CheckboxState};
use aui_icons::{icon, provider_mark, IconName};
use aui_tokens::{scale, ActiveAui, AuiStyled};
use gpui::{div, prelude::*, px, AnyElement, Context, SharedString};
use gpui_kit::base::{h_flex, v_flex};

use crate::app::Harness;
use crate::import::{self, Choice, Found, Plan, CLAUDE, CODEX};
use crate::providers::ProviderId;

/// Sessions per background batch: a batch lands as one store write and one
/// sidebar merge, so the rows appear as the import goes.
const BATCH: usize = 25;
/// A cached plan counts as fresh for ten minutes (the migration cache's
/// rule); renders read it as-is and an older one is recomputed behind it.
const STALE_SECS: u64 = 600;
/// Folder rows the sheet lists before the rest fold into "N more folders"
/// (the board's eight rows).
const LISTED_FOLDERS: usize = 7;
/// The sheet's width on the board.
const SHEET_W: f32 = 540.0;
/// Side padding every sheet block shares (the setup sheet's).
const PAD_X: f32 = 16.0;

/// The plan the surfaces read, with when it was computed.
#[derive(Clone, Debug)]
pub(crate) struct ImportCache {
    pub plan: Plan,
    computed_at: std::time::Instant,
}

/// Where the sheet stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SheetStage {
    /// Folders and the option; Cancel / Import N sessions.
    Choose,
    /// The bar; Close keeps it running.
    Progress,
    /// What landed and what did not.
    Done,
}

/// The open sheet.
#[derive(Clone, Debug)]
pub(crate) struct ImportSheet {
    pub stage: SheetStage,
    /// The controls; `None` until a plan is there to default from.
    pub choice: Option<Choice>,
    /// "Show the J": the unreadable paths are listed.
    pub show_unreadable: bool,
}

/// One import run, in flight or finished.
#[derive(Clone, Debug)]
pub(crate) struct ImportRun {
    /// The journal run id.
    pub id: String,
    /// Sessions chosen.
    pub total: usize,
    /// Sessions handled so far (landed, skipped or unreadable).
    pub done: usize,
    /// The folder in hand, for the progress line.
    pub folder: String,
    /// What is left, with each session's project.
    pub queue: VecDeque<(Found, Option<String>)>,
    /// Ids landed.
    pub added: Vec<String>,
    /// Files that could not be read (scan and execute both).
    pub unreadable: Vec<PathBuf>,
    /// Projects and other folders the landed sessions went into.
    pub projects: HashSet<String>,
    pub other_folders: HashSet<String>,
    /// `import-run:<n>` (captures): stop after this many, mid-run.
    pub hold_after: Option<usize>,
    /// A batch is on the background executor.
    pub in_flight: bool,
    pub finished: bool,
}

/// The import's window state.
#[derive(Debug, Default)]
pub(crate) struct ImportUi {
    pub cache: Option<ImportCache>,
    pub scanning: bool,
    /// A refresh was asked for while one ran (an import or Undo changed
    /// what is in Baaz since it started): run once more when it lands.
    pub rescan_owed: bool,
    pub sheet: Option<ImportSheet>,
    pub journal: import::Journal,
    pub run: Option<ImportRun>,
    /// This launch opened on the first-run connect screen: the only time
    /// the connect screen offers the import.
    pub first_run: bool,
}

impl ImportUi {
    /// The boot state: the journal (empty in a capture, which never writes
    /// it either).
    pub(crate) fn boot() -> Self {
        let journal = if crate::clock::deterministic() {
            import::Journal::default()
        } else {
            import::read_journal(&crate::store::support_dir())
        };
        Self { journal, ..Self::default() }
    }

    /// The cached plan, if any. Never computes.
    pub(crate) fn plan(&self) -> Option<&Plan> {
        self.cache.as_ref().map(|cache| &cache.plan)
    }

    fn stale(&self) -> bool {
        self.cache.as_ref().is_none_or(|cache| cache.computed_at.elapsed().as_secs() >= STALE_SECS)
    }
}

/// `n` with thousands separators: "1,896".
pub(crate) fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", thousands(n), if n == 1 { one } else { many })
}

/// "30 Oct" for a Unix-ms stamp, local time.
fn short_date(at_ms: i64) -> String {
    use chrono::TimeZone as _;
    chrono::Local
        .timestamp_millis_opt(at_ms)
        .single()
        .map(|at| at.format("%-d %b").to_string())
        .unwrap_or_default()
}

/// What a provider's card line says, and which button it carries. Pure over
/// the plan and the journal, so tests drive every state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CardLine {
    pub text: String,
    /// The button's label, `None` for no button.
    pub button: Option<&'static str>,
    /// The quiet (text) button rather than the secondary one.
    pub quiet: bool,
}

/// `provider`'s tally from the cached plan, re-read against the live store:
/// a session that landed since the scan (this run's, mid-run) is in Baaz
/// now, whatever the cache says, so the card never waits on a rescan.
pub(crate) fn live_tally(plan: &Plan, provider: &str, store: &crate::provider_sessions::ProviderSessionStore) -> import::Tally {
    let mut tally = plan.tally(provider);
    let (mut offered, mut scripted) = (0, 0);
    let mut folders: HashSet<&str> = HashSet::new();
    for found in plan.sessions.iter().filter(|f| f.provider == provider) {
        if store.contains_key(&found.session_id) {
            tally.known += 1;
        } else if found.scripted {
            scripted += 1;
        } else {
            offered += 1;
            folders.insert(found.cwd.as_deref().unwrap_or(""));
        }
    }
    tally.offered = offered;
    tally.scripted = scripted;
    tally.folders = folders.len();
    tally
}

/// The card line for one provider's (live) tally, or `None` when its home
/// holds no session.
pub(crate) fn card_line(
    tally: &import::Tally,
    provider: &str,
    imported: (usize, Option<i64>),
) -> Option<CardLine> {
    let tally = *tally;
    let label = if provider == CODEX { "Codex" } else { "Claude Code" };
    let (count, when) = imported;
    if count > 0 {
        let date = when.map(short_date).unwrap_or_default();
        let left = tally.offered;
        let text = if left == 0 {
            format!("{} imported {date} · none left on this Mac", thousands(count))
        } else {
            format!("{} imported {date} · {} left on this Mac", thousands(count), thousands(left))
        };
        return Some(CardLine { text, button: (left > 0).then_some("Import more…"), quiet: true });
    }
    if tally.offered > 0 {
        let none_yet = if tally.known == 0 { " None are in Baaz yet." } else { "" };
        return Some(CardLine {
            text: format!(
                "{} {label} {} on this Mac, in {}.{none_yet}",
                thousands(tally.offered),
                if tally.offered == 1 { "session" } else { "sessions" },
                plural(tally.folders, "folder", "folders"),
            ),
            button: Some("Import…"),
            quiet: false,
        });
    }
    if tally.scripted > 0 {
        let how = if provider == CODEX { "codex exec" } else { "claude -p" };
        let all = if tally.scripted == 1 { "a scripted run" } else { "all scripted runs" };
        return Some(CardLine {
            text: format!(
                "{} {label} {} on this Mac, {all} ({how})",
                thousands(tally.scripted),
                if tally.scripted == 1 { "session" } else { "sessions" },
            ),
            button: None,
            quiet: false,
        });
    }
    None
}

/// The folder rows the sheet shows under `choice`: folders with something to
/// offer, the first [`LISTED_FOLDERS`] whole and the rest folded.
pub(crate) fn visible_folders<'a>(plan: &'a Plan, choice: &Choice) -> (Vec<&'a import::Folder>, Vec<&'a import::Folder>) {
    let shown: Vec<&import::Folder> = plan
        .folders
        .iter()
        .filter(|f| {
            let (c, o) = f.count(choice.skip_scripted);
            c + o > 0
        })
        .collect();
    if shown.len() <= LISTED_FOLDERS + 1 {
        return (shown, Vec::new());
    }
    let rest = shown[LISTED_FOLDERS..].to_vec();
    (shown[..LISTED_FOLDERS].to_vec(), rest)
}

/// The result sentence: "122 sessions imported into 4 projects."
pub(crate) fn result_line(run: &ImportRun) -> String {
    let n = run.added.len();
    if n == 0 {
        return "Nothing new to import: every chosen session is already in Baaz.".to_owned();
    }
    let sessions = plural(n, "session", "sessions");
    let (p, f) = (run.projects.len(), run.other_folders.len());
    match (p, f) {
        (0, 0) => format!("{sessions} imported."),
        (p, 0) => format!("{sessions} imported into {}.", plural(p, "project", "projects")),
        (0, f) => format!("{sessions} imported into {}.", plural(f, "folder", "folders")),
        (p, f) => format!(
            "{sessions} imported into {} and {}.",
            plural(p, "project", "projects"),
            plural(f, "other folder", "other folders")
        ),
    }
}

impl Harness {
    // ------------------------------------------------------------ the cache

    /// Recompute the plan off the UI thread (scan + plan), unless one is in
    /// flight; renders keep reading the old cache meanwhile. Under
    /// `BAAZ_DETERMINISTIC=1` without a fixture owner home nothing is
    /// scanned (see [`import::scan_allowed`]).
    pub(crate) fn refresh_import_scan(&mut self, cx: &mut Context<Self>) {
        if !import::scan_allowed() {
            return;
        }
        if self.import.scanning {
            self.import.rescan_owed = true;
            return;
        }
        self.import.scanning = true;
        let (home, known, projects) = self.import_inputs();
        cx.spawn(async move |this, cx| {
            let plan = cx
                .background_executor()
                .spawn(async move { import::plan(&import::scan_home(&home), &known, &projects, Some(&home)) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.import.scanning = false;
                this.land_import_plan(plan);
                if std::mem::take(&mut this.import.rescan_owed) {
                    this.refresh_import_scan(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Refresh only when the cache is missing or stale.
    pub(crate) fn refresh_import_scan_if_stale(&mut self, cx: &mut Context<Self>) {
        if self.import.stale() {
            self.refresh_import_scan(cx);
        }
    }

    /// What a plan needs from the window: the owner home, the ids Baaz
    /// already has, and the projects as `(id, root)`.
    fn import_inputs(&self) -> (PathBuf, HashSet<String>, import::ProjectRoots) {
        let home = import::owner_home();
        let known: HashSet<String> = self.provider_sessions.keys().cloned().collect();
        let projects = self.projects.sorted().into_iter().map(|p| (p.id.clone(), p.root.clone())).collect();
        (home, known, projects)
    }

    /// A plan landed: cache it, and give an open sheet still waiting on one
    /// its defaults.
    fn land_import_plan(&mut self, plan: Plan) {
        if let Some(sheet) = self.import.sheet.as_mut() {
            if sheet.choice.is_none() {
                sheet.choice = Some(Choice::defaults(&plan));
            }
        }
        self.import.cache = Some(ImportCache { plan, computed_at: std::time::Instant::now() });
    }

    /// The scan, synchronously: the `--steps` verbs only (fixture homes),
    /// so a scripted capture never races the background scan.
    fn scan_import_now(&mut self) {
        if !import::scan_allowed() {
            return;
        }
        let (home, known, projects) = self.import_inputs();
        let plan = import::plan(&import::scan_home(&home), &known, &projects, Some(&home));
        self.land_import_plan(plan);
    }

    // ------------------------------------------------------------ the sheet

    /// Open the import sheet: on the folders, defaulted from the cached
    /// plan (or as soon as one lands), or on the run when one is going.
    pub(crate) fn open_import_sheet(&mut self, cx: &mut Context<Self>) {
        let stage = match &self.import.run {
            Some(run) if !run.finished => SheetStage::Progress,
            _ => SheetStage::Choose,
        };
        if stage == SheetStage::Choose {
            self.import.run = None;
            // "Import more…" reopens on what is left: always a fresh plan.
            self.refresh_import_scan(cx);
        }
        let choice = self.import.plan().map(Choice::defaults);
        self.import.sheet = Some(ImportSheet { stage, choice, show_unreadable: false });
        cx.notify();
    }

    /// Close the sheet (✕, Cancel, Close, Done, Escape). A run keeps going.
    pub(crate) fn close_import_sheet(&mut self, cx: &mut Context<Self>) {
        self.import.sheet = None;
        if self.import.run.as_ref().is_some_and(|run| run.finished) {
            self.import.run = None;
        }
        cx.notify();
    }

    /// Tick or untick one folder (by cwd).
    pub(crate) fn toggle_import_folder(&mut self, cwd: &str, on: bool, cx: &mut Context<Self>) {
        if let Some(choice) = self.import.sheet.as_mut().and_then(|s| s.choice.as_mut()) {
            if on {
                choice.ticked.insert(cwd.to_owned());
            } else {
                choice.ticked.remove(cwd);
            }
        }
        cx.notify();
    }

    /// Tick or untick the folded folders together.
    fn toggle_import_rest(&mut self, on: bool, cx: &mut Context<Self>) {
        let Some(plan) = self.import.plan().cloned() else { return };
        if let Some(choice) = self.import.sheet.as_mut().and_then(|s| s.choice.as_mut()) {
            let (_, rest) = visible_folders(&plan, choice);
            let rest: Vec<String> = rest.iter().map(|f| f.cwd.clone()).collect();
            for cwd in rest {
                if on {
                    choice.ticked.insert(cwd);
                } else {
                    choice.ticked.remove(&cwd);
                }
            }
        }
        cx.notify();
    }

    /// The "Skip scripted runs" box.
    pub(crate) fn set_import_skip_scripted(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(choice) = self.import.sheet.as_mut().and_then(|s| s.choice.as_mut()) {
            choice.skip_scripted = on;
        }
        cx.notify();
    }

    // -------------------------------------------------------------- the run

    /// "Import N sessions": queue the chosen sessions and start the first
    /// batch. `hold_after` stops mid-run (the progress capture only).
    pub(crate) fn start_import(&mut self, hold_after: Option<usize>, cx: &mut Context<Self>) {
        if self.import.run.as_ref().is_some_and(|run| !run.finished) {
            return;
        }
        let Some(plan) = self.import.plan().cloned() else { return };
        let Some(choice) = self.import.sheet.as_ref().and_then(|s| s.choice.clone()) else { return };
        let queue: VecDeque<(Found, Option<String>)> = import::chosen(&plan, &choice)
            .into_iter()
            .filter(|found| !self.provider_sessions.contains_key(&found.session_id))
            .map(|found| {
                let project = plan.folder(found.cwd.as_deref().unwrap_or("")).and_then(|f| f.project.clone());
                (found.clone(), project)
            })
            .collect();
        if queue.is_empty() {
            return;
        }
        // A capture stamps the run with the newest session it imports, not
        // the wall clock, so "N imported <date>" reads the same every day.
        let at_ms = if crate::clock::deterministic() {
            queue.iter().map(|(found, _)| found.updated_ms).max().unwrap_or(0)
        } else {
            crate::usage::now_ms()
        };
        let id = self.import.journal.begin(at_ms);
        let folder = queue.front().map(|(f, _)| folder_label(&plan, f)).unwrap_or_default();
        self.import.run = Some(ImportRun {
            id,
            total: queue.len(),
            done: 0,
            folder,
            queue,
            added: Vec::new(),
            unreadable: plan.unreadable.clone(),
            projects: HashSet::new(),
            other_folders: HashSet::new(),
            hold_after,
            in_flight: false,
            finished: false,
        });
        if let Some(sheet) = self.import.sheet.as_mut() {
            sheet.stage = SheetStage::Progress;
        }
        self.next_import_batch(cx);
        cx.notify();
    }

    /// Run the next batch on the background executor; it lands through
    /// [`Self::land_import_batch`], which starts the one after.
    fn next_import_batch(&mut self, cx: &mut Context<Self>) {
        let Some(run) = self.import.run.as_mut() else { return };
        if run.in_flight || run.finished {
            return;
        }
        let mut size = BATCH;
        if let Some(hold) = run.hold_after {
            size = size.min(hold.saturating_sub(run.done));
        }
        if run.queue.is_empty() {
            self.finish_import(cx);
            return;
        }
        if size == 0 {
            // Held mid-run for a capture: the bar stays where it is.
            return;
        }
        let batch: Vec<(Found, Option<String>)> = run.queue.drain(..size.min(run.queue.len())).collect();
        run.in_flight = true;
        let known: HashSet<String> = self.provider_sessions.keys().cloned().collect();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let result = import::execute_batch(&batch, &known);
                    (batch, result)
                })
                .await;
            let _ = this.update(cx, |this, cx| this.land_import_batch(result.0, result.1, cx));
        })
        .detach();
    }

    /// One batch landed: records into the store (written off the UI
    /// thread), the journal noted, the sidebar merged, the bar moved.
    fn land_import_batch(&mut self, batch: Vec<(Found, Option<String>)>, result: import::BatchResult, cx: &mut Context<Self>) {
        let records = result.records;
        let added = import::land(&mut self.provider_sessions, records);
        let plan = self.import.plan().cloned().unwrap_or_default();
        let Some(run) = self.import.run.as_mut() else { return };
        run.in_flight = false;
        run.done += batch.len();
        run.unreadable.extend(result.unreadable);
        for (found, project) in &batch {
            if !added.contains(&found.session_id) {
                continue;
            }
            match project {
                Some(project) => {
                    run.projects.insert(project.clone());
                }
                None => {
                    run.other_folders.insert(found.cwd.clone().unwrap_or_default());
                }
            }
        }
        run.added.extend(added.iter().cloned());
        if let Some((next, _)) = run.queue.front() {
            run.folder = folder_label(&plan, next);
        }
        let run_id = run.id.clone();
        let sessions: Vec<import::JournalSession> = added
            .iter()
            .filter_map(|id| self.provider_sessions.get(id))
            .map(|record| import::JournalSession { provider: record.provider.clone(), session_id: record.session_id.clone() })
            .collect();
        self.import.journal.note(&run_id, sessions);
        if !added.is_empty() {
            self.persist_provider_sessions(cx);
            self.write_import_journal();
            if self.merge_provider_rows() {
                cx.notify();
            }
        }
        self.next_import_batch(cx);
        cx.notify();
    }

    /// The run is over: the search rows through the P3 path, the journal,
    /// the toast with Undo, a fresh plan for the cards.
    fn finish_import(&mut self, cx: &mut Context<Self>) {
        let Some(run) = self.import.run.as_mut() else { return };
        run.finished = true;
        let (id, added) = (run.id.clone(), run.added.len());
        self.import.journal.drop_empty();
        self.write_import_journal();
        if let Some(sheet) = self.import.sheet.as_mut() {
            sheet.stage = SheetStage::Done;
        } else {
            // Closed mid-run: nothing waits on the result.
            self.import.run = None;
        }
        if added > 0 {
            self.rebuild_search_index(cx);
            self.push_undo(
                crate::app::UndoBatch::Imported(id),
                format!("{} imported", plural(added, "session", "sessions")),
                String::new(),
                cx,
            );
        }
        self.refresh_import_scan(cx);
        cx.notify();
    }

    /// Undo run `id`: remove exactly the records it added (never a file),
    /// write the store and the journal, re-merge the sidebar and the search
    /// rows, re-plan.
    pub(crate) fn undo_import(&mut self, id: &str, cx: &mut Context<Self>) {
        let removed = import::undo(&mut self.provider_sessions, &mut self.import.journal, id);
        self.write_import_journal();
        if !removed.is_empty() {
            self.persist_provider_sessions(cx);
            self.merge_provider_rows();
            self.invalidate_list();
            self.rebuild_search_index(cx);
        }
        if self.import.run.as_ref().is_some_and(|run| run.id == id) {
            self.import.run = None;
            self.import.sheet = None;
        }
        self.refresh_import_scan(cx);
        cx.notify();
    }

    /// The journal to disk, never in a capture.
    fn write_import_journal(&self) {
        if crate::clock::deterministic() {
            return;
        }
        import::write_journal(&crate::store::support_dir(), &self.import.journal);
    }

    /// "Show in sidebar": close the sheet and Settings, and reveal the first
    /// imported row.
    fn show_imported_in_sidebar(&mut self, cx: &mut Context<Self>) {
        let first = self.import.run.as_ref().and_then(|run| run.added.first().cloned());
        self.close_import_sheet(cx);
        if self.settings_page.open {
            self.close_settings(cx);
        }
        if let Some(id) = first {
            self.reveal = Some(id);
        }
        cx.notify();
    }

    // ---------------------------------------------------------------- steps

    /// `import-open`: the sheet, on a plan scanned now (fixture homes).
    pub(crate) fn step_import_open(&mut self, cx: &mut Context<Self>) {
        self.scan_import_now();
        self.open_import_sheet(cx);
        // The background refresh `open` kicked is redundant here; the
        // synchronous plan is already the defaults.
        if let (Some(sheet), Some(plan)) = (self.import.sheet.as_mut(), self.import.cache.as_ref()) {
            if sheet.choice.is_none() {
                sheet.choice = Some(Choice::defaults(&plan.plan));
            }
        }
        if self.import.sheet.as_ref().is_none_or(|s| s.choice.is_none()) {
            crate::steps::record_step_failure("import-open");
        }
    }

    /// `import-toggle:<folder>`: flip one folder (its name or cwd), or
    /// `scripted` for the "Skip scripted runs" box.
    pub(crate) fn step_import_toggle(&mut self, rest: &str, cx: &mut Context<Self>) {
        let rest = rest.trim();
        let Some(choice) = self.import.sheet.as_ref().and_then(|s| s.choice.clone()) else {
            crate::steps::record_step_failure(&format!("import-toggle:{rest}"));
            return;
        };
        if rest == "scripted" {
            self.set_import_skip_scripted(!choice.skip_scripted, cx);
            return;
        }
        let cwd = self
            .import
            .plan()
            .and_then(|plan| plan.folders.iter().find(|f| f.cwd == rest || f.name == rest).map(|f| f.cwd.clone()));
        match cwd {
            Some(cwd) => {
                let on = !choice.ticked.contains(&cwd);
                self.toggle_import_folder(&cwd, on, cx);
            }
            None => crate::steps::record_step_failure(&format!("import-toggle:{rest}")),
        }
    }

    /// `import-run[:<n>]`: press Import; with `n`, hold after `n` sessions.
    pub(crate) fn step_import_run(&mut self, rest: &str, cx: &mut Context<Self>) {
        let hold = rest.trim().parse::<usize>().ok();
        self.start_import(hold, cx);
        if self.import.run.is_none() {
            crate::steps::record_step_failure(&format!("import-run:{rest}"));
        }
    }

    /// `import-undo`: the toast's Undo for the newest run.
    pub(crate) fn step_import_undo(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.import.journal.runs.last().map(|run| run.id.clone()) else {
            crate::steps::record_step_failure("import-undo");
            return;
        };
        self.undo_stack.retain(|batch| *batch != crate::app::UndoBatch::Imported(id.clone()));
        self.undo_import(&id, cx);
    }

    // --------------------------------------------------------------- render

    /// The import line under a Claude Code / Codex Settings card, or `None`
    /// (no cache yet, no sessions, or not one of the two).
    pub(crate) fn import_card_line(&self, id: ProviderId, cx: &mut Context<Self>) -> Option<AnyElement> {
        let provider = match id {
            ProviderId::ClaudeCode => CLAUDE,
            ProviderId::Codex => CODEX,
            ProviderId::Muse => return None,
        };
        let plan = self.import.plan()?;
        let live = live_tally(plan, provider, &self.provider_sessions);
        let line = card_line(&live, provider, self.import.journal.imported(provider, &self.provider_sessions))?;
        let p = cx.aui().colors;
        let mut row = h_flex()
            .id(SharedString::from(format!("import-card-line-{provider}")))
            .w_full()
            .items_center()
            .gap(px(10.0))
            .px(px(14.0))
            .py(px(12.0))
            .text_color(p.ink_2)
            .child(icon(IconName::Clock).size(px(14.0)).color(p.ink_2))
            .child(div().flex_1().min_w(px(0.0)).text_px(scale::FS_12).line_height(px(18.0)).child(line.text.clone()));
        if let Some(label) = line.button {
            let open = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.open_import_sheet(cx));
            let mut btn = button(SharedString::from(format!("import-card-open-{provider}")), label)
                .size(ButtonSize::Row)
                .accessibility_label(format!("{} · {label}", id.label()))
                .on_click(move |event, window, cx| open(event, window, cx));
            if line.quiet {
                btn = btn.ghost();
            }
            row = row.child(btn);
        }
        Some(row.into_any_element())
    }

    /// The connect screen's assist runs for the import offer: "Found N
    /// earlier sessions on this Mac. Import…" — first run only, once a
    /// provider with sessions not in Baaz is Connected.
    pub(crate) fn import_connect_assist(&self) -> Vec<aui::screens::AssistRun> {
        if !self.import.first_run {
            return Vec::new();
        }
        let Some(plan) = self.import.plan() else { return Vec::new() };
        let connected = |id: ProviderId| {
            self.connect_statuses
                .iter()
                .any(|s| s.provider == id && s.headline() == crate::provider_status::Headline::Connected)
        };
        let mut n = 0;
        if connected(ProviderId::ClaudeCode) {
            n += plan.claude.offered;
        }
        if connected(ProviderId::Codex) {
            n += plan.codex.offered;
        }
        if n == 0 {
            return Vec::new();
        }
        vec![
            aui::screens::AssistRun::Text(SharedString::from(format!(
                "Found {} on this Mac. ",
                plural(n, "earlier session", "earlier sessions")
            ))),
            aui::screens::AssistRun::Link(SharedString::from(IMPORT_ASSIST_KEY), SharedString::from("Import…")),
        ]
    }

    /// The sheet over the scrim, or `None` while it is shut.
    pub(crate) fn render_import_sheet(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sheet = self.import.sheet.as_ref()?;
        let p = cx.aui().colors;
        let escape = cx.listener(|this: &mut Self, event: &gpui::KeyDownEvent, _, cx| {
            if event.keystroke.key == "escape" {
                this.close_import_sheet(cx);
                cx.stop_propagation();
            }
        });
        let title = if sheet.stage == SheetStage::Done { "Import finished" } else { "Import sessions" };
        let close_x = {
            let close = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.close_import_sheet(cx));
            icon_button("import-sheet-close", IconName::X)
                .ghost()
                .accessibility_label(format!("Close {title}"))
                .on_click(move |event, window, cx| close(event, window, cx))
        };
        let head = h_flex()
            .w_full()
            .h(px(44.0))
            .flex_none()
            .items_center()
            .gap(px(10.0))
            .pl(px(PAD_X))
            .pr(px(8.0))
            .border_b_1()
            .border_color(p.line)
            .child(div().flex_1().min_w(px(0.0)).truncate().text_px(scale::FS_13).semibold().text_color(p.ink).child(title))
            .child(close_x);
        let (body, actions) = match sheet.stage {
            SheetStage::Choose => self.import_choose_body(sheet, cx),
            SheetStage::Progress => self.import_progress_body(cx),
            SheetStage::Done => self.import_done_body(sheet, cx),
        };
        let card = v_flex()
            .id("import-sheet")
            .w(px(SHEET_W))
            .max_w_full()
            .rounded(px(12.0))
            .bg(p.surface_2)
            .shadow(p.floating_shadow())
            .overflow_hidden()
            .text_color(p.ink)
            .role(gpui::Role::Dialog)
            .aria_label(title)
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(head)
            .children(body)
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(PAD_X))
                    .pt(px(14.0))
                    .pb(px(16.0))
                    .child(div().flex_1())
                    .children(actions),
            );
        Some(
            div()
                .absolute()
                .inset_0()
                .capture_key_down(move |event, window, cx| escape(event, window, cx))
                .child(aui::screens::sheet_scrim("import-sheet-scrim", card, None))
                .into_any_element(),
        )
    }

    /// The choose stage: intro, folders, the option, the note; Cancel /
    /// Import N sessions.
    fn import_choose_body(&self, sheet: &ImportSheet, cx: &mut Context<Self>) -> (Vec<AnyElement>, Vec<AnyElement>) {
        let p = cx.aui().colors;
        let cancel = {
            let close = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.close_import_sheet(cx));
            button("import-sheet-cancel", "Cancel")
                .size(ButtonSize::Row)
                .accessibility_label("Cancel the import")
                .on_click(move |event, window, cx| close(event, window, cx))
                .into_any_element()
        };
        let (Some(plan), Some(choice)) = (self.import.plan(), sheet.choice.as_ref()) else {
            let waiting = div()
                .px(px(PAD_X))
                .pt(px(12.0))
                .text_px(scale::FS_12)
                .line_height(px(19.0))
                .text_color(p.ink_2)
                .child("Looking for earlier sessions on this Mac…")
                .into_any_element();
            let go = button("import-sheet-go", "Import")
                .size(ButtonSize::Row)
                .primary()
                .disabled(true)
                .accessibility_label("Import, waiting for the scan")
                .into_any_element();
            return (vec![waiting], vec![cancel, go]);
        };
        let mut body = Vec::new();
        body.push(
            div()
                .px(px(PAD_X))
                .pt(px(12.0))
                .text_px(scale::FS_12)
                .line_height(px(19.0))
                .text_color(p.ink_2)
                .child(
                    "Earlier Claude Code and Codex sessions, by the folder they ran in. Folders that are Baaz \
                     projects are ticked.",
                )
                .into_any_element(),
        );
        let (listed, rest) = visible_folders(plan, choice);
        let mut list = v_flex()
            .id("import-folders")
            .mx(px(PAD_X))
            .mt(px(10.0))
            .rounded(px(8.0))
            .bg(p.surface_3)
            .shadow(p.edge())
            .overflow_hidden()
            .role(gpui::Role::List)
            .aria_label("Folders")
            .child(
                h_flex()
                    .h(px(30.0))
                    .px(px(12.0))
                    .gap(px(10.0))
                    .items_center()
                    .border_b_1()
                    .border_color(p.line)
                    .text_px(scale::FS_11)
                    .text_color(p.ink_4)
                    .child(div().flex_1().child("Folder"))
                    .child("Sessions"),
            );
        if listed.is_empty() {
            list = list.child(
                div()
                    .h(px(36.0))
                    .px(px(12.0))
                    .flex()
                    .items_center()
                    .text_px(scale::FS_12)
                    .text_color(p.ink_2)
                    .child(if plan.sessions.is_empty() {
                        "No earlier sessions on this Mac that are not in Baaz already."
                    } else {
                        "Only scripted runs are left. Untick Skip scripted runs to see them."
                    }),
            );
        }
        for (n, folder) in listed.iter().enumerate() {
            let on = choice.ticked.contains(&folder.cwd);
            let (c, o) = folder.count(choice.skip_scripted);
            let cwd = folder.cwd.clone();
            let flip = cx.listener(move |this: &mut Self, intent: &CheckboxIntent, _, cx| {
                let CheckboxIntent::Set(state) = intent;
                this.toggle_import_folder(&cwd, state.checked(), cx);
            });
            let state = if on { CheckboxState::On } else { CheckboxState::Off };
            let tag = folder.project.is_some().then_some("project");
            list = list.child(folder_row(
                SharedString::from(format!("import-folder-{n}")),
                state,
                &folder.name,
                &folder.parent,
                tag,
                (c, o),
                n > 0,
                move |intent, window, cx| flip(&intent, window, cx),
                cx,
            ));
        }
        if !rest.is_empty() {
            let ticked = rest.iter().filter(|f| choice.ticked.contains(&f.cwd)).count();
            let state = match ticked {
                0 => CheckboxState::Off,
                n if n == rest.len() => CheckboxState::On,
                _ => CheckboxState::Mixed,
            };
            let (c, o) = rest.iter().fold((0, 0), |(c, o), f| {
                let (fc, fo) = f.count(choice.skip_scripted);
                (c + fc, o + fo)
            });
            let flip = cx.listener(move |this: &mut Self, intent: &CheckboxIntent, _, cx| {
                let CheckboxIntent::Set(state) = intent;
                this.toggle_import_rest(state.checked(), cx);
            });
            list = list.child(folder_row(
                SharedString::from("import-folder-rest"),
                state,
                &format!("{} more folders", thousands(rest.len())),
                "",
                None,
                (c, o),
                !listed.is_empty(),
                move |intent, window, cx| flip(&intent, window, cx),
                cx,
            ));
        }
        body.push(list.into_any_element());
        let scripted = plan.scripted();
        let skip = choice.skip_scripted;
        let flip_skip = cx.listener(move |this: &mut Self, intent: &CheckboxIntent, _, cx| {
            let CheckboxIntent::Set(state) = intent;
            this.set_import_skip_scripted(state.checked(), cx);
        });
        body.push(
            h_flex()
                .id("import-skip-scripted-row")
                .items_center()
                .gap(px(8.0))
                .px(px(PAD_X))
                .pt(px(10.0))
                .text_px(scale::FS_12)
                .child(
                    checkbox("import-skip-scripted", if skip { CheckboxState::On } else { CheckboxState::Off }, false)
                        .accessibility_label(format!("Skip scripted runs, {} of them", thousands(scripted)))
                        .on_intent(move |intent, window, cx| flip_skip(&intent, window, cx)),
                )
                .child(div().text_color(p.ink).child("Skip scripted runs"))
                .child(
                    div()
                        .text_color(p.ink_2)
                        .child(format!("(claude -p, codex exec · {})", thousands(scripted))),
                )
                .into_any_element(),
        );
        body.push(
            div()
                .px(px(PAD_X))
                .pt(px(10.0))
                .text_px(scale::FS_12)
                .line_height(px(18.0))
                .text_color(p.ink_4)
                .child(
                    "They stay where they are: nothing is copied or moved, and Claude Code and Codex keep using \
                     them. Continuing one in Baaz adds to the same session.",
                )
                .into_any_element(),
        );
        let n = import::chosen(plan, choice)
            .into_iter()
            .filter(|found| !self.provider_sessions.contains_key(&found.session_id))
            .count();
        let label = format!("Import {}", plural(n, "session", "sessions"));
        let go = {
            let start = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.start_import(None, cx));
            button("import-sheet-go", label.clone())
                .size(ButtonSize::Row)
                .primary()
                .disabled(n == 0)
                .accessibility_label(label)
                .on_click(move |event, window, cx| start(event, window, cx))
                .into_any_element()
        };
        (body, vec![cancel, go])
    }

    /// The progress stage: the line, the bar, the note; Close.
    fn import_progress_body(&self, cx: &mut Context<Self>) -> (Vec<AnyElement>, Vec<AnyElement>) {
        let p = cx.aui().colors;
        let (done, total, folder) = self
            .import
            .run
            .as_ref()
            .map(|run| (run.done, run.total, run.folder.clone()))
            .unwrap_or((0, 0, String::new()));
        let fraction = if total == 0 { 0.0 } else { done as f32 / total as f32 };
        let line = h_flex()
            .id("import-progress-line")
            .px(px(PAD_X))
            .pt(px(16.0))
            .gap(px(4.0))
            .text_px(scale::FS_13)
            .line_height(px(21.0))
            .role(gpui::Role::Status)
            .aria_label(format!("Importing {} of {}, {folder}", thousands(done), thousands(total)))
            .child(div().text_color(p.ink).child(format!("Importing {} of {}…", thousands(done), thousands(total))))
            .child(div().text_color(p.ink_2).child(folder));
        let bar = div()
            .id("import-progress-bar")
            .mx(px(PAD_X))
            .mt(px(16.0))
            .h(px(6.0))
            .rounded(px(3.0))
            .bg(p.surface_3)
            .shadow(p.edge())
            .overflow_hidden()
            .role(gpui::Role::ProgressIndicator)
            .aria_label(format!("{} of {} imported", thousands(done), thousands(total)))
            .child(div().h_full().w(gpui::relative(fraction)).rounded(px(3.0)).bg(p.ink).opacity(0.75));
        let note = div()
            .px(px(PAD_X))
            .pt(px(10.0))
            .text_px(scale::FS_12)
            .line_height(px(18.0))
            .text_color(p.ink_4)
            .child(
                "Reading each session\u{2019}s first prompt and words for search. You can close this; it keeps \
                 going, and the sessions appear in the sidebar as they land.",
            );
        let close = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.close_import_sheet(cx));
        let close = button("import-sheet-close-run", "Close")
            .size(ButtonSize::Row)
            .accessibility_label("Close, the import keeps going")
            .on_click(move |event, window, cx| close(event, window, cx))
            .into_any_element();
        (vec![line.into_any_element(), bar.into_any_element(), note.into_any_element()], vec![close])
    }

    /// The result stage: what landed, what did not; Show in sidebar / Done.
    fn import_done_body(&self, sheet: &ImportSheet, cx: &mut Context<Self>) -> (Vec<AnyElement>, Vec<AnyElement>) {
        let p = cx.aui().colors;
        let Some(run) = self.import.run.as_ref() else { return (Vec::new(), Vec::new()) };
        let unreadable = run.unreadable.len();
        let mut big = h_flex()
            .id("import-result")
            .flex_wrap()
            .px(px(PAD_X))
            .pt(px(16.0))
            .gap(px(4.0))
            .text_px(scale::FS_13)
            .line_height(px(21.0))
            .child(div().text_color(p.ink).child(result_line(run)));
        if unreadable > 0 {
            big = big.child(div().text_color(p.ink_2).child(format!(
                "{} couldn\u{2019}t be read and {} left out.",
                thousands(unreadable),
                if unreadable == 1 { "was" } else { "were" }
            )));
        }
        let mut note = h_flex()
            .px(px(PAD_X))
            .pt(px(10.0))
            .gap(px(4.0))
            .text_px(scale::FS_12)
            .line_height(px(18.0))
            .text_color(p.ink_4);
        if unreadable > 0 {
            let toggle = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                if let Some(sheet) = this.import.sheet.as_mut() {
                    sheet.show_unreadable = !sheet.show_unreadable;
                }
                cx.notify();
            });
            let label = if sheet.show_unreadable {
                "Hide them".to_owned()
            } else {
                format!("Show the {}", thousands(unreadable))
            };
            note = note
                .child(
                    div()
                        .id("import-show-unreadable")
                        .cursor_pointer()
                        .underline()
                        .text_color(p.ink_2)
                        .hover(|s| s.text_color(p.ink))
                        .role(gpui::Role::Button)
                        .aria_label(format!("{label}, the files that could not be read"))
                        .on_click(move |event, window, cx| toggle(event, window, cx))
                        .child(label),
                )
                .child("·");
        }
        note = note.child("Import more any time from Settings \u{203a} Providers.");
        let mut body = vec![big.into_any_element(), note.into_any_element()];
        if sheet.show_unreadable && unreadable > 0 {
            let mut list = v_flex()
                .id("import-unreadable")
                .mx(px(PAD_X))
                .mt(px(8.0))
                .p(px(10.0))
                .gap(px(2.0))
                .rounded(px(8.0))
                .bg(p.surface_3)
                .shadow(p.edge())
                .role(gpui::Role::List)
                .aria_label("Files that could not be read");
            for path in run.unreadable.iter().take(12) {
                list = list.child(
                    div()
                        .truncate()
                        .font_family(scale::FONT_MONO)
                        .text_px(11.0)
                        .text_color(p.ink_2)
                        .child(crate::layout::tilde_path(path)),
                );
            }
            if unreadable > 12 {
                list = list.child(div().text_px(scale::FS_11).text_color(p.ink_4).child(format!("and {} more", unreadable - 12)));
            }
            body.push(list.into_any_element());
        }
        let show = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.show_imported_in_sidebar(cx));
        let done = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.close_import_sheet(cx));
        let mut actions = Vec::new();
        if !run.added.is_empty() {
            actions.push(
                button("import-sheet-show", "Show in sidebar")
                    .size(ButtonSize::Row)
                    .accessibility_label("Show the imported sessions in the sidebar")
                    .on_click(move |event, window, cx| show(event, window, cx))
                    .into_any_element(),
            );
        }
        actions.push(
            button("import-sheet-done", "Done")
                .size(ButtonSize::Row)
                .primary()
                .accessibility_label("Done")
                .on_click(move |event, window, cx| done(event, window, cx))
                .into_any_element(),
        );
        (body, actions)
    }
}

/// The connect assist link's key that opens the sheet.
pub(crate) const IMPORT_ASSIST_KEY: &str = "import-open";

/// The progress line's folder label for `found`.
fn folder_label(plan: &Plan, found: &Found) -> String {
    plan.folder(found.cwd.as_deref().unwrap_or("")).map(|f| f.name.clone()).unwrap_or_default()
}

/// One folder row: checkbox, name with its faint parent path, the
/// "project" tag, per-provider counts with each provider's mark.
#[allow(clippy::too_many_arguments)]
fn folder_row(
    id: SharedString,
    state: CheckboxState,
    name: &str,
    parent: &str,
    tag: Option<&'static str>,
    counts: (usize, usize),
    hairline: bool,
    on_intent: impl Fn(CheckboxIntent, &mut gpui::Window, &mut gpui::App) + 'static,
    cx: &mut Context<Harness>,
) -> AnyElement {
    let p = cx.aui().colors;
    let label = if parent.is_empty() { name.to_owned() } else { format!("{name}, in {parent}") };
    let mut row = h_flex()
        .id(id.clone())
        .h(px(36.0))
        .px(px(12.0))
        .gap(px(10.0))
        .items_center()
        .text_px(scale::FS_12)
        .role(gpui::Role::ListItem)
        .aria_label(SharedString::from(label.clone()));
    if hairline {
        row = row.border_t_1().border_color(p.line);
    }
    row = row
        .child(
            checkbox(SharedString::from(format!("{id}-box")), state, false)
                .accessibility_label(format!("Import {label}"))
                .on_intent(on_intent),
        )
        .child(
            h_flex()
                .flex_1()
                .min_w(px(0.0))
                .gap(px(6.0))
                .items_baseline()
                .overflow_hidden()
                .child(div().flex_none().text_color(p.ink).child(name.to_owned()))
                .child(
                    div()
                        .min_w(px(0.0))
                        .truncate()
                        .font_family(scale::FONT_MONO)
                        .text_px(11.0)
                        .text_color(p.ink_4)
                        .child(parent.to_owned()),
                ),
        );
    if let Some(tag) = tag {
        row = row.child(div().flex_none().text_px(10.5).text_color(p.ink_4).child(tag));
    }
    for (provider, n) in [(ProviderId::ClaudeCode, counts.0), (ProviderId::Codex, counts.1)] {
        if n == 0 {
            continue;
        }
        row = row.child(
            h_flex()
                .flex_none()
                .gap(px(5.0))
                .items_center()
                .text_color(p.ink_2)
                .child(provider_mark(provider.icon()).size(px(14.0)))
                .child(thousands(n)),
        );
    }
    row.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::{Folder, Tally};


    #[test]
    fn the_card_line_follows_the_board() {
        let fresh = Tally { offered: 1896, scripted: 31, known: 0, folders: 37 };
        assert_eq!(
            card_line(&fresh, CLAUDE, (0, None)),
            Some(CardLine {
                text: "1,896 Claude Code sessions on this Mac, in 37 folders. None are in Baaz yet.".into(),
                button: Some("Import…"),
                quiet: false,
            })
        );
        assert_eq!(card_line(&Tally::default(), CODEX, (0, None)), None, "no sessions, no line");
        let scripted = Tally { offered: 0, scripted: 95, known: 0, folders: 0 };
        assert_eq!(
            card_line(&scripted, CODEX, (0, None)),
            Some(CardLine {
                text: "95 Codex sessions on this Mac, all scripted runs (codex exec)".into(),
                button: None,
                quiet: false,
            }),
            "never an Import button that imports nothing (docs/72 §4)"
        );
        let after = Tally { offered: 1774, scripted: 31, known: 122, folders: 33 };
        let at = chrono::NaiveDate::from_ymd_opt(2026, 10, 30)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_local_timezone(chrono::Local)
            .unwrap()
            .timestamp_millis();
        assert_eq!(
            card_line(&after, CLAUDE, (122, Some(at))),
            Some(CardLine {
                text: "122 imported 30 Oct · 1,774 left on this Mac".into(),
                button: Some("Import more…"),
                quiet: true,
            })
        );
    }

    #[test]
    fn the_live_tally_counts_what_landed_since_the_scan() {
        let found = |id: &str, scripted: bool, cwd: &str| Found {
            provider: CLAUDE,
            session_id: id.into(),
            path: PathBuf::from("/x"),
            cwd: Some(cwd.into()),
            created_ms: 0,
            updated_ms: 0,
            first_prompt: Some("p".into()),
            scripted,
        };
        let plan = Plan {
            sessions: vec![found("a", false, "/w1"), found("b", false, "/w2"), found("c", true, "/w1")],
            claude: Tally { offered: 2, scripted: 1, known: 4, folders: 2 },
            ..Plan::default()
        };
        let mut store = crate::provider_sessions::ProviderSessionStore::new();
        crate::provider_sessions::upsert_open(&mut store, CLAUDE, "a", None, None, None);
        let live = live_tally(&plan, CLAUDE, &store);
        assert_eq!(live, Tally { offered: 1, scripted: 1, known: 5, folders: 1 });
    }

    #[test]
    fn folders_fold_after_seven_and_hide_when_empty_under_the_option() {
        let folder = |n: usize, claude: usize, scripted: usize| Folder {
            cwd: format!("/f{n}"),
            name: format!("f{n}"),
            parent: "~".into(),
            project: None,
            claude,
            codex: 0,
            claude_scripted: scripted,
            codex_scripted: 0,
            latest_ms: 0,
        };
        let mut plan = Plan { folders: (0..10).map(|n| folder(n, 1, 0)).collect(), ..Plan::default() };
        plan.folders.push(folder(10, 0, 3));
        let skip = Choice { skip_scripted: true, ticked: HashSet::new() };
        let (listed, rest) = visible_folders(&plan, &skip);
        assert_eq!((listed.len(), rest.len()), (7, 3), "an all-scripted folder hides with the box ticked");
        let all = Choice { skip_scripted: false, ticked: HashSet::new() };
        let (listed, rest) = visible_folders(&plan, &all);
        assert_eq!((listed.len(), rest.len()), (7, 4));
        plan.folders.truncate(8);
        let (listed, rest) = visible_folders(&plan, &skip);
        assert_eq!((listed.len(), rest.len()), (8, 0), "eight fit: no fold row for one folder");
    }

    #[test]
    fn the_result_line_names_projects_and_folders() {
        let mut run = ImportRun {
            id: "r".into(),
            total: 3,
            done: 3,
            folder: String::new(),
            queue: VecDeque::new(),
            added: vec!["a".into(), "b".into()],
            unreadable: Vec::new(),
            projects: ["p1".to_owned()].into_iter().collect(),
            other_folders: HashSet::new(),
            hold_after: None,
            in_flight: false,
            finished: true,
        };
        assert_eq!(result_line(&run), "2 sessions imported into 1 project.");
        run.other_folders.insert("/x".into());
        assert_eq!(result_line(&run), "2 sessions imported into 1 project and 1 other folder.");
        assert_eq!(thousands(1896), "1,896");
        assert_eq!(thousands(122), "122");
        assert_eq!(thousands(1_000_000), "1,000,000");
    }
}
