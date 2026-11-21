//! A session row's one menu (docs/82 8.5): Rename, Pin, Fork, Continue in…,
//! Move to project, Reveal folder, Archive and Delete…, on the row's `⋯` and
//! on a right-click. The library draws the rows the host offers
//! ([`RowMenuOffers`]); this module decides which work for a session and
//! does what each row says.
//!
//! - **Fork** and **Continue in…** act on an open session: the menu opens
//!   the session first and then its fork picker, or its provider menu —
//!   which reads "Continue in…" on a session that has started, since
//!   picking another provider there is a hand-off.
//! - **Move to project** changes where the sidebar lists the session (its
//!   stored project id), never its folder: a session keeps running where it
//!   started. The submenu says so.
//! - **Delete…** is the forget path ([`Harness::delete_provider_session`]),
//!   offered only for a session Baaz keeps the record of (Muse lists its
//!   own; Baaz only hides or archives those), after a confirm that names
//!   what goes.
//!
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;

use aui::screens::sidebar::{MoveTarget, RowMenuOffers};

/// What the row menu does after it opens a session: the action needs the
/// session in front.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowFollowUp {
    /// The fork picker (the header menu's Fork).
    Fork,
    /// The composer's provider menu, "Continue in…".
    ContinueIn,
}

/// The confirm's words for Delete…: what the forget path removes
/// ([`Harness::delete_provider_session`]) and what it only offers.
pub(crate) const DELETE_DETAIL: &str = "Baaz forgets it for good: its turns and attachments, the pictures you sent in it, \
    its hand-off snapshot and its checkpoints. If its transcript is in Baaz's folder, you are asked next whether to move it \
    to the Trash.";

/// How long a follow-up waits for a session that opens later (a provider
/// session reopening off the UI thread), polling every `FOLLOW_UP_TICK`.
const FOLLOW_UP_TICK: std::time::Duration = std::time::Duration::from_millis(50);
const FOLLOW_UP_TICKS: u32 = 100;

impl Harness {
    /// What `session_id`'s row menu offers (docs/82 8.5, rule 13: a row
    /// only where it works). Fork where its provider can fork and it has a
    /// turn to fork from; Continue in… where it has started and another
    /// provider is on; Move to project where there is somewhere else to list
    /// it (Unfiled only when its folder is no project's root — clearing the
    /// stored project would otherwise file it there again); Reveal folder
    /// where its folder exists; Delete… where Baaz keeps its record and it
    /// is no hand-off chain. An archived row offers Unarchive, Reveal folder
    /// and Delete… only.
    pub(crate) fn row_menu_offers(&self, session_id: &str, cx: &gpui::App) -> RowMenuOffers {
        let list = self.session_list.read(cx);
        let Some(entry) = list.sessions.iter().find(|entry| entry.id == session_id) else {
            return RowMenuOffers::default();
        };
        let archived = entry.archived;
        let live_turns = self.find_view(session_id, cx).is_some_and(|view| view.read(cx).rt(cx).has_turns());
        let started = entry.turns > 0 || entry.turns_unknown || live_turns;
        let fork = !archived && started && crate::providers::offers(entry.provider, provider::Capability::ForkSession);
        let continue_in = !archived
            && started
            && crate::settings_providers::live_visible_provider_ids().iter().any(|id| *id != entry.provider);
        let reveal = entry
            .workspace
            .as_deref()
            .map(str::trim)
            .filter(|folder| !folder.is_empty())
            .is_some_and(|folder| std::path::Path::new(folder).is_dir());
        let delete = list.provider_sessions.contains_key(session_id) && self.chain_members(session_id, cx).len() <= 1;
        let move_to = if archived { Vec::new() } else { move_targets(entry, &list.projects) };
        RowMenuOffers { fork, continue_in, move_to, reveal, delete, archived }
    }

    /// The row menu's Move to project: list `session_id` under `project`
    /// (`None`: Unfiled). Only where it is listed changes — its stored
    /// project id; the session keeps its folder. A target the menu would
    /// not offer changes nothing.
    pub(crate) fn move_session_to_project(&mut self, session_id: &str, project: Option<String>, cx: &mut Context<Self>) -> bool {
        let offered = self.row_menu_offers(session_id, cx).move_to.iter().any(|target| target.project.as_deref() == project.as_deref());
        if !offered {
            return false;
        }
        self.set_override(session_id, |meta| meta.project = project.clone(), cx);
        self.invalidate_list(cx);
        true
    }

    /// Fork or Continue in… from the row menu: open `session_id` (a row
    /// click, no reveal) and then `then` on it. A session that opens later
    /// (a provider session reopening) gets it when it lands — a few seconds
    /// at most; a click elsewhere meanwhile wins and nothing follows.
    pub(crate) fn open_then(&mut self, session_id: String, then: RowFollowUp, window: &mut Window, cx: &mut Context<Self>) {
        let open = |this: &Self, cx: &gpui::App| this.active_id(cx).map(|id| this.chain_head(&id, cx)).as_deref() == Some(session_id.as_str());
        if !open(self, cx) {
            self.resume_quiet(session_id.clone(), window, cx);
        }
        if open(self, cx) {
            self.row_follow_up(then, cx);
            return;
        }
        self.tasks.push(cx.spawn(async move |this, cx| {
            for _ in 0..FOLLOW_UP_TICKS {
                cx.background_executor().timer(FOLLOW_UP_TICK).await;
                let done = this
                    .update(cx, |this, cx| {
                        let front = this.active_id(cx).map(|id| this.chain_head(&id, cx));
                        if front.as_deref() == Some(session_id.as_str()) {
                            this.row_follow_up(then, cx);
                            return true;
                        }
                        // Another session was asked for: the follow-up is moot.
                        this.open_state.pending_id.as_deref() != Some(session_id.as_str())
                    })
                    .unwrap_or(true);
                if done {
                    break;
                }
            }
        }));
    }

    /// The follow-up on the session in front.
    fn row_follow_up(&mut self, then: RowFollowUp, cx: &mut Context<Self>) {
        match then {
            RowFollowUp::Fork => {
                self.show_palette(crate::overlays::PaletteKind::Fork, cx);
            }
            RowFollowUp::ContinueIn => {
                if let Some(view) = self.active.clone() {
                    view.update(cx, |view, cx| view.open_provider_menu(cx));
                }
            }
        }
    }

    /// The row menu's Reveal folder: the session's folder selected in
    /// Finder, off the UI thread. No folder does nothing.
    pub(crate) fn reveal_session_folder(&mut self, session_id: &str, cx: &mut Context<Self>) -> bool {
        let folder = self
            .session_list
            .read(cx)
            .sessions
            .iter()
            .find(|entry| entry.id == session_id)
            .and_then(|entry| entry.workspace.clone())
            .filter(|folder| !folder.trim().is_empty());
        match folder {
            Some(folder) => {
                crate::open_in::reveal_folder(std::path::PathBuf::from(folder));
                true
            }
            None => false,
        }
    }

    /// The row menu's Delete…: a danger confirm naming what goes. Only
    /// its own Delete button deletes ([`Self::confirm_delete_dialog`]).
    pub(crate) fn open_delete_dialog(&mut self, session_id: String, cx: &mut Context<Self>) {
        let label = self
            .collapsed_head_label(&session_id, cx)
            .unwrap_or_else(|| sidebar::UNNAMED.to_owned());
        self.set_dialog(
            cx,
            Dialog {
                title: format!("Delete \u{201C}{label}\u{201D}?"),
                detail: DELETE_DETAIL.into(),
                kind: DialogKind::Warning,
                primary: "Delete",
                action: DialogAction::DeleteSession,
                archive_target: Some(session_id),
            },
        );
    }

    /// The Delete confirm's button: the session is forgotten
    /// ([`Self::delete_provider_session`]), and when it was the open one the
    /// newest remaining session opens in its place, as Archive does.
    pub(crate) fn confirm_delete_dialog(&mut self, target: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.close_dialog(cx);
        let Some(session_id) = target else { return };
        if !self.session_list.read(cx).provider_sessions.contains_key(&session_id) {
            return;
        }
        let was_open = self.active_id(cx).map(|id| self.chain_head(&id, cx)).as_deref() == Some(session_id.as_str());
        self.delete_provider_session(&session_id, cx);
        if was_open && self.providers.shared_ready() {
            if let Some(next) = self.visible_sessions(cx).first().map(|entry| entry.id.clone()) {
                self.resume(next, window, cx);
            }
        }
        cx.notify();
    }

    #[cfg(feature = "probe")]
    /// `fixture-record:<session_id>`: capture aid — the local record a real
    /// row of a provider that keeps no list of its own carries (the
    /// sidebar fixture's rows have none), so its menu offers Delete…. Only
    /// in Baaz's memory: nothing is written. No such row, a row of the
    /// provider that lists its own sessions, or one already recorded fails
    /// the step.
    pub(crate) fn step_fixture_record(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let id = rest.trim();
        let entry = self
            .session_list
            .read(cx)
            .sessions
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
            .ok_or_else(|| format!("no session row `{id}`"))?;
        if entry.provider.has(provider::ProviderTrait::NativeSessionList) {
            return Err(format!("`{id}`'s provider lists its own sessions: Baaz keeps no record of it"));
        }
        if self.session_list.read(cx).provider_sessions.contains_key(id) {
            return Err(format!("`{id}` already has a record"));
        }
        let record = crate::provider_sessions::ProviderSessionRecord {
            provider: entry.provider.as_str().to_owned(),
            session_id: id.to_owned(),
            workspace: entry.workspace.clone(),
            created_ms: entry.created.timestamp_millis(),
            updated_ms: entry.updated.timestamp_millis(),
            turns: entry.turns,
            origin: None,
        };
        self.session_list.update(cx, |list, _| list.provider_sessions.insert(id.to_owned(), record));
        Ok(())
    }

    #[cfg(feature = "probe")]
    /// `row-action:[<session_id>,]<action>`: a row menu row, as its click
    /// does, on that row (the open session's with no id): `fork`,
    /// `continue-in`, `archive` (the confirm, or Unarchive), `delete` (the
    /// confirm), `move:<project name|unfiled>`. A row the menu does not
    /// offer for that session fails the step; so does `reveal`, which opens
    /// Finder and is never scripted.
    pub(crate) fn step_row_action(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let (named, action) = match rest.split_once(',') {
            Some((id, action)) => (id.trim(), action.trim()),
            None => ("", rest.trim()),
        };
        let id = if named.is_empty() {
            self.active_id(cx).map(|id| self.chain_head(&id, cx)).ok_or("row-action: no session is open and none was named")?
        } else {
            named.to_owned()
        };
        if !self.session_list.read(cx).sessions.iter().any(|entry| entry.id == id) {
            return Err(format!("no session row `{id}`"));
        }
        let offers = self.row_menu_offers(&id, cx);
        let refused = |what: &str| Err(format!("`{id}`'s menu offers no {what}"));
        match action {
            "fork" if offers.fork => self.open_then(id, RowFollowUp::Fork, window, cx),
            "fork" => return refused("Fork"),
            "continue-in" if offers.continue_in => self.open_then(id, RowFollowUp::ContinueIn, window, cx),
            "continue-in" => return refused("Continue in…"),
            "archive" if offers.archived => self.unarchive_session(id, cx),
            "archive" => self.open_archive_dialog(id, cx),
            "delete" if offers.delete => self.open_delete_dialog(id, cx),
            "delete" => return refused("Delete…"),
            "reveal" => return Err("row-action: reveal opens Finder and is never scripted".into()),
            other => match other.strip_prefix("move:") {
                Some(name) => {
                    let target = offers
                        .move_to
                        .iter()
                        .find(|target| target.name.eq_ignore_ascii_case(name.trim()))
                        .ok_or_else(|| format!("`{id}` cannot move to `{name}`"))?;
                    let project = target.project.as_ref().map(|p| p.to_string());
                    if !self.move_session_to_project(&id, project, cx) {
                        return Err(format!("`{id}` did not move to `{name}`"));
                    }
                }
                None => return Err(format!("row-action: unknown action `{other}`")),
            },
        }
        Ok(())
    }
}

/// Where a row can be listed: every available project (the one it is
/// listed under checked), and Unfiled when its folder is no project's root.
/// Empty when there is nowhere else to go.
fn move_targets(entry: &SessionEntry, projects: &crate::projects::Projects) -> Vec<MoveTarget> {
    let mut targets: Vec<MoveTarget> = projects
        .sorted()
        .into_iter()
        .filter(|project| projects.is_available(&project.id))
        .map(|project| MoveTarget {
            project: Some(project.id.clone().into()),
            name: project.name.clone().into(),
            current: entry.project.as_deref() == Some(project.id.as_str()),
        })
        .collect();
    if projects.resolve_available(entry.workspace.as_deref(), None).is_none() {
        targets.push(MoveTarget { project: None, name: sidebar::UNFILED_LABEL.into(), current: entry.project.is_none() });
    }
    if targets.iter().filter(|target| !target.current).count() == 0 {
        return Vec::new();
    }
    targets
}
