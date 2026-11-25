//! Undo in a session (docs/82 6.4), the runtime half: "Undo this turn" on a
//! finished turn and "Revert file" from Changes, each confirmed first, each
//! taken back by its toast's Undo.
//!
//! The git work is [`crate::undo`] (checked first, a safety snapshot under
//! `refs/baaz/<uuid>/undo/<n>` before anything changes); here it runs off
//! the UI thread, one action at a time, and the session keeps what the
//! person sees of it in its [`UndoBook`]:
//!
//! * which turns can be undone — those with both checkpoints in the folder
//!   (read from the refs when the lane opens and each time a turn's refs
//!   land), never while a turn runs here, never on a replayed or read-only
//!   lane, never in a capture (no checkpoints there: rule 13, absent);
//! * the confirm waiting on the person ([`UndoAsk`]): the window's dialog
//!   carries its number, so only its own button confirms it;
//! * what was done, for the toast's Undo;
//! * the quiet note under an undone turn;
//! * what the provider is told: a short preface on the wire text of the
//!   next message sent here (never the bubble). Taking an action back
//!   before that message cancels its preface; after it, a matching note
//!   goes with the next one.
//!
//! The application hears [`SessionEvent::UndoConfirm`] (put the dialog
//! up) and [`SessionEvent::UndoLanded`] (the toast, and Changes re-reads).

use super::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The most files a confirm or a preface names before "and N more".
const NAMED_FILES: usize = 8;

/// What an action puts back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UndoAction {
    /// One finished turn's own change, `pre..post`; `number` is its place
    /// among the session's agent turns.
    Turn { turn_id: String, number: usize, pre: String, post: String },
    /// One file back to `HEAD`.
    FileToHead { path: String },
    /// One file's part of one turn's change.
    FileInTurn { path: String, turn_id: String, number: usize, pre: String, post: String },
}

/// A confirm waiting on the person.
#[derive(Clone, Debug)]
pub(crate) struct UndoAsk {
    /// What the dialog's target carries: only this confirm runs it.
    pub seq: u64,
    pub action: UndoAction,
    /// The folder it runs in.
    pub folder: PathBuf,
    /// docs/85 13.3: an agent's `changes_revert` asked, and waits on this
    /// for the answer (reverted, declined, refused). An agent's ask may run
    /// while its own turn does (the agent is blocked on the call), and its
    /// revert tells the provider nothing more: the agent asked for it.
    pub waiter: Option<std::sync::mpsc::Sender<Result<serde_json::Value, String>>>,
}

impl UndoAsk {
    fn by_agent(&self) -> bool {
        self.waiter.is_some()
    }

    fn answer(&self, outcome: Result<serde_json::Value, String>) {
        if let Some(waiter) = &self.waiter {
            let _ = waiter.send(outcome);
        }
    }
}

/// One action done here, for the toast's Undo.
#[derive(Clone, Debug)]
struct Done {
    applied: crate::undo::Applied,
    action: UndoAction,
    folder: PathBuf,
}

/// A session's undo state (see the module doc). Lives on its runtime.
#[derive(Default)]
pub(crate) struct UndoBook {
    /// Turn ref components with both checkpoints, as last read.
    finished: HashSet<String>,
    reading: bool,
    /// A read was asked for while one ran.
    stale: bool,
    /// What the fold rows offer: `finished` less what was undone here.
    offered: Rc<HashSet<String>>,
    /// Undone turns (by id): the note under each.
    undone: Rc<HashMap<String, String>>,
    done: Vec<Done>,
    /// What the next message tells the provider, by action number.
    preface: Vec<(u64, String)>,
    asked: Option<UndoAsk>,
    seq: u64,
    /// An action or a confirm's read is running.
    busy: bool,
}

impl UndoBook {
    fn reoffer(&mut self) {
        let undone: HashSet<String> = self.undone.keys().map(|id| crate::checkpoints::component(id)).collect();
        self.offered = Rc::new(self.finished.iter().filter(|c| !undone.contains(*c)).cloned().collect());
    }
}

/// `a.rs, b.rs and 3 more`.
pub(crate) fn name_files(files: &[String]) -> String {
    let named: Vec<&str> = files.iter().take(NAMED_FILES).map(String::as_str).collect();
    let more = files.len().saturating_sub(NAMED_FILES);
    match (named.as_slice(), more) {
        ([], _) => String::new(),
        ([one], 0) => (*one).to_owned(),
        (all, 0) => format!("{} and {}", all[..all.len() - 1].join(", "), all[all.len() - 1]),
        (all, more) => format!("{} and {more} more", all.join(", ")),
    }
}

/// A path's last part: what a title names.
fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn files_word(n: usize) -> String {
    if n == 1 { "1 file".to_owned() } else { format!("{n} files") }
}

/// The confirm's title, body and primary button.
pub(crate) fn confirm_copy(action: &UndoAction, files: &[String]) -> (String, String, &'static str) {
    match action {
        UndoAction::Turn { number, .. } => (
            "Undo this turn?".to_owned(),
            format!(
                "{} {} back to how {} before turn {number}: {}. Your other edits stay, and the agent is told with your next message.",
                capitalised(&files_word(files.len())),
                if files.len() == 1 { "goes" } else { "go" },
                if files.len() == 1 { "it was" } else { "they were" },
                name_files(files)
            ),
            "Undo",
        ),
        UndoAction::FileToHead { path } => (
            format!("Revert {}?", file_name(path)),
            format!("{path} goes back to the last commit, and its uncommitted changes are discarded."),
            "Revert",
        ),
        UndoAction::FileInTurn { path, number, .. } => (
            format!("Revert {}?", file_name(path)),
            format!("{path} loses what turn {number} changed in it. Other edits to it stay."),
            "Revert",
        ),
    }
}

fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// What the provider is told with the next message after `action`.
pub(crate) fn preface_for(action: &UndoAction, files: &[String]) -> String {
    match action {
        UndoAction::Turn { .. } => format!(
            "Note: before this message, the person undid your changes from an earlier turn (files: {}). Those files are back to how they were before that turn.",
            name_files(files)
        ),
        UndoAction::FileToHead { path } => {
            format!("Note: before this message, the person reverted {path} to its last committed version, discarding its uncommitted changes.")
        }
        UndoAction::FileInTurn { path, .. } => format!(
            "Note: before this message, the person reverted your change to {path} from an earlier turn. That part of the file is back to how it was before that turn."
        ),
    }
}

/// What the provider is told after an action was taken back once its own
/// preface had already gone.
pub(crate) fn redo_preface(files: &[String]) -> String {
    format!(
        "Note: before this message, the person restored changes they had undone earlier (files: {}). Those files are as they were before the undo.",
        name_files(files)
    )
}

/// The toast after `action` landed: title and body.
pub(crate) fn landed_copy(action: &UndoAction, files: &[String]) -> (String, String) {
    match action {
        UndoAction::Turn { .. } => ("Turn undone".to_owned(), format!("{} restored", files_word(files.len()))),
        UndoAction::FileToHead { path } => (format!("Reverted {}", file_name(path)), "Back to the last commit".to_owned()),
        UndoAction::FileInTurn { path, number, .. } => (format!("Reverted {}", file_name(path)), format!("Back to before turn {number}")),
    }
}

/// The toast when `action` changed nothing.
pub(crate) fn refused_copy(action: &UndoAction, refusal: &crate::undo::Refusal) -> (String, String) {
    use crate::undo::Refusal;
    let title = match action {
        UndoAction::Turn { .. } => "Couldn't undo this turn".to_owned(),
        UndoAction::FileToHead { path } | UndoAction::FileInTurn { path, .. } => format!("Couldn't revert {}", file_name(path)),
    };
    let body = match refusal {
        Refusal::Conflicts(files) if files.len() == 1 => format!("{} changed since, so nothing was changed.", files[0]),
        Refusal::Conflicts(files) => format!("{} changed since, so nothing was changed.", name_files(files)),
        Refusal::Nothing => "There is nothing to put back.".to_owned(),
        Refusal::Unprotected(files) => format!("{} is ignored by git, so it could not be brought back afterwards. Nothing was changed.", name_files(files)),
        Refusal::Failed(why) => format!("Git said: {why}. Nothing was changed."),
    };
    (title, body)
}

/// The quiet note under an undone turn.
pub(crate) fn undone_note(files: usize) -> String {
    format!("Turn undone · {} restored", files_word(files))
}

/// The session's uuid from `baaz.db` (blocks: off the UI thread).
pub(super) fn session_key(provider: &str, id: &str) -> Option<String> {
    let (provider, id) = (provider.to_owned(), id.to_owned());
    let state = crate::store::support_dir();
    crate::db::read(crate::db::path_in(&state), move |db| crate::db::sessions::read_row(&db.conn, &provider, &id).ok().flatten()).flatten()
}

/// A turn's two checkpoints, read from the refs (blocks).
fn turn_ends(folder: &Path, key: &str, turn_id: &str) -> Option<(String, String)> {
    let refs = crate::checkpoints::session_refs(folder, key)?;
    let found = refs.get(&crate::checkpoints::component(turn_id))?;
    Some((found.pre.clone()?, found.post.clone()?))
}

impl SessionRuntime {
    /// Whether Undo can act here now: this session takes checkpoints, its
    /// lane is live, nothing runs and no action is in flight.
    pub(crate) fn undo_ready(&self) -> bool {
        !self.replay && !self.read_only && self.checkpoints.folder().is_some() && !self.working() && !self.undo.busy
    }

    /// The turns (by ref component) whose fold row offers "Undo turn":
    /// `None` while nothing can be undone now.
    pub(crate) fn undo_offered(&self) -> Option<Rc<HashSet<String>>> {
        (self.undo_ready() && !self.undo.offered.is_empty()).then(|| Rc::clone(&self.undo.offered))
    }

    /// Whether the turn `turn_id` can be undone now.
    pub(crate) fn turn_undoable(&self, turn_id: &str) -> bool {
        self.undo_offered().is_some_and(|offered| offered.contains(&crate::checkpoints::component(turn_id)))
    }

    /// The newest agent turn that can be undone now: the session menu's
    /// "Undo last turn".
    pub(crate) fn last_undoable_turn(&self) -> Option<String> {
        let offered = self.undo_offered()?;
        self.assistant_turn_ids().into_iter().rev().find(|id| offered.contains(&crate::checkpoints::component(id)))
    }

    /// The notes under undone turns, by turn id.
    pub(crate) fn undone_notes(&self) -> Rc<HashMap<String, String>> {
        Rc::clone(&self.undo.undone)
    }

    /// Re-read which turns have both checkpoints (when the lane opens and
    /// when a turn's refs land). One read at a time.
    pub(super) fn refresh_undoable(&mut self, cx: &mut Context<Self>) {
        let Some(folder) = self.checkpoints.folder().map(Path::to_path_buf) else { return };
        if self.replay || self.read_only {
            return;
        }
        if self.undo.reading {
            self.undo.stale = true;
            return;
        }
        self.undo.reading = true;
        self.undo.stale = false;
        let known = self.checkpoints.key().map(str::to_owned);
        let (provider, id) = (self.provider_kind().as_str().to_owned(), self.session_id.clone());
        cx.spawn(async move |this, cx| {
            let finished = cx
                .background_spawn(async move {
                    let key = known.or_else(|| session_key(&provider, &id))?;
                    let refs = crate::checkpoints::session_refs(&folder, &key)?;
                    Some(refs.into_iter().filter(|(_, ends)| ends.pre.is_some() && ends.post.is_some()).map(|(turn, _)| turn).collect::<HashSet<String>>())
                })
                .await;
            let _ = this.update(cx, |this: &mut Self, cx| {
                this.undo.reading = false;
                if let Some(finished) = finished {
                    this.undo.finished = finished;
                    this.undo.reoffer();
                    cx.emit(RuntimeEvent::UndoChanged);
                }
                if std::mem::take(&mut this.undo.stale) {
                    this.refresh_undoable(cx);
                }
            });
        })
        .detach();
    }

    /// "Undo this turn" on `turn_id`: its files are read, then the confirm
    /// goes up. `Err` (and nothing asked) when that turn cannot be undone now.
    pub(crate) fn ask_undo_turn(&mut self, turn_id: &str, cx: &mut Context<Self>) -> Result<(), String> {
        if !self.turn_undoable(turn_id) {
            return Err(format!("turn {turn_id} cannot be undone now"));
        }
        let folder = self.checkpoints.folder().map(Path::to_path_buf).ok_or("no checkpoints here")?;
        let number = self.assistant_turn_ids().iter().position(|id| id == turn_id).map_or(0, |i| i + 1);
        let known = self.checkpoints.key().map(str::to_owned);
        let (provider, id, turn) = (self.provider_kind().as_str().to_owned(), self.session_id.clone(), turn_id.to_owned());
        self.undo.busy = true;
        cx.spawn(async move |this, cx| {
            let dir = folder.clone();
            let read = cx
                .background_spawn(async move {
                    let key = known.or_else(|| session_key(&provider, &id)).ok_or("no session record")?;
                    let (pre, post) = turn_ends(&dir, &key, &turn).ok_or("the turn's checkpoints are gone")?;
                    let files = crate::undo::turn_files(&dir, &pre, &post).map_err(|refusal| refusal.to_string())?;
                    Ok::<_, String>((UndoAction::Turn { turn_id: turn, number, pre, post }, files))
                })
                .await;
            let _ = this.update(cx, |this: &mut Self, cx| {
                this.undo.busy = false;
                match read {
                    Ok((action, files)) if !files.is_empty() => this.put_up_confirm(action, files, folder, None, cx),
                    Ok((action, _)) => this.undo_refused(&action, &crate::undo::Refusal::Nothing, cx),
                    Err(why) => crate::baaz_log!("undo: the turn's files could not be read ({why})"),
                }
                cx.emit(RuntimeEvent::UndoChanged);
            });
        })
        .detach();
        cx.emit(RuntimeEvent::UndoChanged);
        Ok(())
    }

    /// "Revert file" from Changes: `path` (top-relative) in `folder`, to
    /// `HEAD` (`turn` None) or out of the turn `turn` (its id, number and
    /// ends, as the Changes screen read them). `Err` when Undo cannot act.
    pub(crate) fn ask_revert_file(&mut self, folder: &Path, path: &str, turn: Option<(String, usize, String, String)>, cx: &mut Context<Self>) -> Result<(), String> {
        if !self.undo_ready() {
            return Err("Revert is not offered here now".into());
        }
        let action = match turn {
            None => UndoAction::FileToHead { path: path.to_owned() },
            Some((turn_id, number, pre, post)) => UndoAction::FileInTurn { path: path.to_owned(), turn_id, number, pre, post },
        };
        self.put_up_confirm(action, vec![path.to_owned()], folder.to_path_buf(), None, cx);
        Ok(())
    }

    /// docs/85 13.3: an agent's `changes_revert`, through the same confirm.
    /// Unlike the person's Revert it may be asked while the session's turn
    /// runs — that turn is the agent waiting on this call. `waiter` hears
    /// the outcome.
    pub(crate) fn ask_revert_file_for_agent(
        &mut self,
        folder: &Path,
        path: &str,
        turn: Option<(String, usize, String, String)>,
        waiter: std::sync::mpsc::Sender<Result<serde_json::Value, String>>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.replay || self.read_only || self.checkpoints.folder().is_none() || self.undo.busy {
            return Err("Revert is not offered in this session now (no checkpoints here, or an undo is running)".into());
        }
        let action = match turn {
            None => UndoAction::FileToHead { path: path.to_owned() },
            Some((turn_id, number, pre, post)) => UndoAction::FileInTurn { path: path.to_owned(), turn_id, number, pre, post },
        };
        self.put_up_confirm(action, vec![path.to_owned()], folder.to_path_buf(), Some(waiter), cx);
        Ok(())
    }

    /// The confirm numbered `seq` was dismissed (Cancel, Escape, the
    /// scrim): an agent waiting on it hears "declined". Nothing changed.
    pub(crate) fn decline_undo(&mut self, seq: u64) {
        if self.undo.asked.as_ref().is_some_and(|ask| ask.seq == seq) {
            let ask = self.undo.asked.take().expect("checked");
            ask.answer(Ok(serde_json::json!({"declined": true, "note": "the person said no; nothing was changed"})));
        }
    }

    /// Keep the ask and have the window put its dialog up.
    fn put_up_confirm(
        &mut self,
        action: UndoAction,
        files: Vec<String>,
        folder: PathBuf,
        waiter: Option<std::sync::mpsc::Sender<Result<serde_json::Value, String>>>,
        cx: &mut Context<Self>,
    ) {
        self.undo.seq += 1;
        let seq = self.undo.seq;
        let (title, detail, primary) = confirm_copy(&action, &files);
        if let Some(old) = self.undo.asked.take() {
            old.answer(Err("another confirm replaced this one; nothing was changed".into()));
        }
        self.undo.asked = Some(UndoAsk { seq, action, folder, waiter });
        cx.emit(SessionEvent::UndoConfirm { session_id: self.session_id.clone(), seq, title, detail, primary });
    }

    /// The confirm's primary: run the ask numbered `seq`. `Err` when it is
    /// not the one waiting, or Undo cannot act now (a turn started).
    pub(crate) fn confirm_undo(&mut self, seq: u64, cx: &mut Context<Self>) -> Result<(), String> {
        if self.undo.asked.as_ref().is_none_or(|ask| ask.seq != seq) {
            return Err("that confirm is no longer waiting".into());
        }
        let ask = self.undo.asked.take().expect("checked");
        let ready = if ask.by_agent() { !self.replay && !self.read_only && !self.undo.busy } else { self.undo_ready() };
        if !ready {
            ask.answer(Err("Undo cannot act now; nothing was changed".into()));
            return Err("Undo cannot act now".into());
        }
        let known = self.checkpoints.key().map(str::to_owned);
        let (provider, id) = (self.provider_kind().as_str().to_owned(), self.session_id.clone());
        self.undo.busy = true;
        cx.emit(RuntimeEvent::UndoChanged);
        cx.spawn(async move |this, cx| {
            let (action, folder) = (ask.action.clone(), ask.folder.clone());
            let result = cx
                .background_spawn(async move {
                    let key = known.or_else(|| session_key(&provider, &id)).ok_or_else(|| crate::undo::Refusal::Failed("this session has no saved record yet".into()))?;
                    match &action {
                        UndoAction::Turn { pre, post, .. } => crate::undo::undo_turn(&folder, &key, pre, post),
                        UndoAction::FileToHead { path } => crate::undo::revert_to_head(&folder, &key, path),
                        UndoAction::FileInTurn { path, pre, post, .. } => crate::undo::revert_in_turn(&folder, &key, pre, post, path),
                    }
                })
                .await;
            let _ = this.update(cx, |this: &mut Self, cx| {
                this.undo.busy = false;
                match result {
                    Ok(applied) => {
                        ask.answer(Ok(serde_json::json!({"reverted": applied.files, "note": "the person can take it back from the toast"})));
                        let by_agent = ask.by_agent();
                        this.undo_landed(Done { applied, action: ask.action, folder: ask.folder }, by_agent, cx);
                    }
                    Err(refusal) => {
                        ask.answer(Err(format!("not reverted: {refusal}")));
                        this.undo_refused(&ask.action, &refusal, cx);
                    }
                }
                cx.emit(RuntimeEvent::UndoChanged);
            });
        })
        .detach();
        Ok(())
    }

    /// `by_agent`: the agent asked for it, so no preface tells it again.
    fn undo_landed(&mut self, done: Done, by_agent: bool, cx: &mut Context<Self>) {
        let files = done.applied.files.clone();
        let n = done.applied.n;
        if let UndoAction::Turn { turn_id, .. } = &done.action {
            Rc::make_mut(&mut self.undo.undone).insert(turn_id.clone(), undone_note(files.len()));
            self.undo.reoffer();
        }
        if !by_agent {
            self.undo.preface.push((n, preface_for(&done.action, &files)));
        }
        let (title, body) = landed_copy(&done.action, &files);
        self.undo.done.retain(|old| old.applied.n != n);
        self.undo.done.push(done);
        let keep = crate::undo::KEEP;
        if self.undo.done.len() > keep {
            let extra = self.undo.done.len() - keep;
            self.undo.done.drain(..extra);
        }
        cx.emit(SessionEvent::UndoLanded { session_id: self.session_id.clone(), title, body, undo: Some(n) });
    }

    fn undo_refused(&mut self, action: &UndoAction, refusal: &crate::undo::Refusal, cx: &mut Context<Self>) {
        crate::baaz_log!("undo: {action:?} refused ({refusal})");
        let (title, body) = refused_copy(action, refusal);
        cx.emit(SessionEvent::UndoLanded { session_id: self.session_id.clone(), title, body, undo: None });
    }

    /// The toast's Undo: take the action numbered `n` back. `Err` when it
    /// is not one done here, or Undo cannot act now.
    pub(crate) fn take_back(&mut self, n: u64, cx: &mut Context<Self>) -> Result<(), String> {
        let Some(done) = self.undo.done.iter().find(|done| done.applied.n == n).cloned() else {
            return Err(format!("no action {n} to take back"));
        };
        if self.working() || self.undo.busy {
            return Err("Undo cannot act now".into());
        }
        self.undo.busy = true;
        cx.emit(RuntimeEvent::UndoChanged);
        cx.spawn(async move |this, cx| {
            let (folder, applied) = (done.folder.clone(), done.applied.clone());
            let result = cx.background_spawn(async move { crate::undo::restore(&folder, &applied) }).await;
            let _ = this.update(cx, |this: &mut Self, cx| {
                this.undo.busy = false;
                match result {
                    Ok(files) => this.taken_back(done, files, cx),
                    Err(refusal) => {
                        crate::baaz_log!("undo: taking back {n} refused ({refusal})");
                        let (_, body) = refused_copy(&done.action, &refusal);
                        cx.emit(SessionEvent::UndoLanded { session_id: this.session_id.clone(), title: "Couldn't take the undo back".into(), body, undo: None });
                    }
                }
                cx.emit(RuntimeEvent::UndoChanged);
            });
        })
        .detach();
        Ok(())
    }

    fn taken_back(&mut self, done: Done, files: Vec<String>, cx: &mut Context<Self>) {
        let n = done.applied.n;
        self.undo.done.retain(|old| old.applied.n != n);
        if let UndoAction::Turn { turn_id, .. } = &done.action {
            Rc::make_mut(&mut self.undo.undone).remove(turn_id);
            self.undo.reoffer();
        }
        // Not sent yet: the provider never hears of it. Sent: it hears the
        // undo was taken back.
        let before = self.undo.preface.len();
        self.undo.preface.retain(|(pending, _)| *pending != n);
        if self.undo.preface.len() == before {
            self.undo.preface.push((n, redo_preface(&files)));
        }
        cx.emit(SessionEvent::UndoLanded {
            session_id: self.session_id.clone(),
            title: "Undo taken back".into(),
            body: format!("{} as they were", files_word(files.len())),
            undo: None,
        });
    }

    /// What the next message tells the provider, taken: every note waiting,
    /// in order. `None` when there is none.
    pub(super) fn take_undo_preface(&mut self) -> Option<Vec<(u64, String)>> {
        let taken = std::mem::take(&mut self.undo.preface);
        (!taken.is_empty()).then_some(taken)
    }

    /// A send that never left: its notes wait for the next one.
    pub(super) fn put_back_undo_preface(&mut self, taken: Vec<(u64, String)>) {
        let later = std::mem::replace(&mut self.undo.preface, taken);
        self.undo.preface.extend(later);
    }

    /// The probe's `undo-turn:` in a capture: the confirm from fixture data
    /// (the last agent turn's edited files), with no git read — a capture
    /// runs in the owner's repository, and its confirm cannot apply
    /// (captures take no checkpoints, so [`Self::confirm_undo`] refuses).
    #[cfg(feature = "probe")]
    pub(crate) fn fixture_undo_confirm(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let ids = self.assistant_turn_ids();
        let (number, turn_id, files) = ids
            .iter()
            .enumerate()
            .rev()
            .map(|(i, id)| (i + 1, id.clone(), self.turn_edits(id)))
            .find(|(_, _, files)| !files.is_empty())
            .ok_or("no agent turn in this transcript edited a file")?;
        let root = self.workspace_path().trim_end_matches('/').to_owned();
        let files: Vec<String> = files.iter().map(|f| f.strip_prefix(&format!("{root}/")).unwrap_or(f).to_owned()).collect();
        let action = UndoAction::Turn { turn_id, number, pre: "fixture-pre".into(), post: "fixture-post".into() };
        self.put_up_confirm(action, files, PathBuf::from(root), None, cx);
        Ok(())
    }

    /// The probe's `revert-file:` in a capture: the confirm for `path`
    /// (back to the last commit), from fixture data. See
    /// [`Self::fixture_undo_confirm`].
    #[cfg(feature = "probe")]
    pub(crate) fn fixture_revert_confirm(&mut self, folder: &Path, path: &str, cx: &mut Context<Self>) {
        self.put_up_confirm(UndoAction::FileToHead { path: path.to_owned() }, vec![path.to_owned()], folder.to_path_buf(), None, cx);
    }
}
