//! Opening sessions: new sessions and the per-project drafts, resuming
//! a listed session, the immediate swap that answers a click on its own
//! frame (`activate`), and the MRU of parked views that makes reopening
//! instant (park/evict, `close_view`).
//!
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;

/// How many parked session views the MRU keeps (folds keep an MRU of eight
/// too, so a parked view's own fold never grows past it either). A session
/// past the cap with work in hand keeps its runtime, not its view
/// (docs/82 4.8).
pub(super) const SESSION_CACHE_LIMIT: usize = 8;

/// What `new_session_in` does about the target project's draft.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DraftDecision {
    /// Reopen this draft session's view; nothing starts.
    Reuse(String),
    /// Start a session; first forget this stale draft name, if any.
    Start { stale: Option<String> },
}

/// The draft map's decision, in pure form: reuse the project's draft while
/// `live` still calls it unsent, else start — pruning a name the app can no
/// longer open.
pub(crate) fn draft_decision(
    drafts: &HashMap<String, String>,
    project: &str,
    live: impl Fn(&str) -> bool,
) -> DraftDecision {
    match drafts.get(project) {
        Some(named) if live(named) => DraftDecision::Reuse(named.clone()),
        Some(named) => DraftDecision::Start { stale: Some(named.clone()) },
        None => DraftDecision::Start { stale: None },
    }
}

/// Which parked ids survive the MRU cap, in order: `drafts` (every view the
/// caller must keep — drafts and busy views) are never evicted, everything
/// else keeps most-recent-first up to `limit`.
pub(crate) fn evict_parked(
    ids: &[String],
    drafts: &std::collections::HashSet<String>,
    limit: usize,
) -> Vec<String> {
    let mut kept = 0usize;
    ids.iter()
        .filter(|id| {
            if drafts.contains(*id) {
                true
            } else {
                kept += 1;
                kept <= limit
            }
        })
        .cloned()
        .collect()
}

/// The id prefix of a local draft: a Muse draft with no session behind it
/// (scripted chrome, or a live launch with Muse still starting).
pub(crate) const LOCAL_DRAFT_PREFIX: &str = "local-draft-";

/// Whether `session_id` names a local draft ([`LOCAL_DRAFT_PREFIX`]).
pub(crate) fn is_local_draft(session_id: &str) -> bool {
    session_id.starts_with(LOCAL_DRAFT_PREFIX)
}

impl Harness {
    /// `session/start` in the current project, on the switcher's provider.
    ///
    /// A new session is also the moment to re-walk the workspace: files come
    /// and go while the window is open, and the `@` picker should not offer a
    /// path that was deleted an hour ago. With no adoption there is nowhere
    /// to start, so nothing starts.
    pub(crate) fn new_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.current_project_id();
        self.new_session_in(current, window, cx);
    }

    /// The cached muse `model/list` ids across the open and parked views:
    /// what a muse start checks its stored default against. Empty means
    /// nothing is known yet — never a reason to drop.
    pub(super) fn muse_catalog_ids(&self, cx: &gpui::App) -> Vec<String> {
        self.registry
            .live(self.active.as_ref())
            .iter()
            .flat_map(|view| view.read(cx).rt(cx).cached_muse_catalog_ids())
            .collect()
    }

    pub(crate) fn new_session_in(
        &mut self,
        project: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // B2a-H2b: like `resume_inner` — ⌘N, the rail's new cell, the
        // sidebar button and the provider picks switch out of Browse
        // before the session starts. (⌘T opens a person tab while in
        // Browse and never reaches here.)
        if crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse {
            self.set_mode(crate::layout::Mode::Sessions, window, cx);
        }
        // A new session (possibly in another project) drops the session tab
        // the Browse centre held and leaves the address field, even when the
        // mode never changed and `set_mode` early-returned.
        self.browser.update(cx, |browser, cx| browser.set_session_tab(None, cx));
        self.leave_browse_address(window, cx);
        // A `new` while a provider switch is pending is the person doing
        // something else: the switch is cancelled (its open lands stale
        // and is discarded) and this opens as a normal new session on the
        // restored provider, the old draft staying with the old view.
        // `BAAZ_TRACE=1`: the click every new-session mark measures from
        // (`new-draft-shown` when the draft paints on it, `provider lane
        // open` when a provider child answered first).
        crate::log::trace_reset();
        crate::log::trace_mark("new");
        let replaced = self.open_state.replacing_id().map(str::to_owned);
        let switch_cancelled = self.cancel_pending_switch(cx);
        // One session per `new`: a switch already in flight owns the next
        // session — a second `new` (boot racing the head's `new`, a double
        // ⌘N, a scripted repeat) starts nothing, unless a provider switch
        // claimed it for its replacement (see [`OpenState::admit_new`]).
        if !self.open_state.admit_new() {
            crate::baaz_log!("new: a session is already opening; keeping it");
            self.browser.update(cx, |browser, _| browser.drop_untargeted_parcels());
            return;
        }
        if switch_cancelled {
            // This `new` opens fresh: drop the replaced view's draft-name
            // so the reuse below cannot mistake this explicit new session
            // for a repeated ⌘N on the still-unsent draft (the restored
            // provider is the old view's own, so it would serve). The text
            // itself stays in the old view, which parks below.
            if let Some(old) = replaced {
                self.open_state.forget_draft(&old);
            }
        }
        self.new_session_open(project, window, cx, None);
    }

    /// The `SwitchProvider` task's start: runs only while `epoch` is still
    /// the current switch. A superseding switch, or any other navigation
    /// that cancelled this one, moved the epoch on — so the task starts
    /// nothing (and consumes nobody's claim), and the open, if it already
    /// left, lands stale and is discarded.
    pub(super) fn new_session_for_switch(&mut self, epoch: u64, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open_state.switch_is_current(epoch) {
            return;
        }
        if self.open_state.take_claim_for(epoch) {
            // A folder pick moves the draft into its project (docs/82 6.6).
            let project = self.open_state.folder_pick.take().or_else(|| self.current_project_id());
            self.new_session_open(project, window, cx, Some(epoch));
        }
    }

    fn new_session_open(
        &mut self,
        project: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
        switch: Option<u64>,
    ) {
        if let Some(id) = project.as_deref().filter(|id| self.session_list.read(cx).projects.find_available(id).is_some()) {
            self.session_list.update(cx, |list, _| list.projects.touch(id));
            self.session_list.update(cx, |list, _| list.projects.current = Some(id.to_owned()));
            self.current_project = Some(id.to_owned());
            projects::write(&self.session_list.read(cx).projects);
            // A new session in another project moves the page's root (D55).
            if self.skills_open(cx) {
                self.relist_skills(cx);
            }
        }
        // A missing root is nowhere to start: fall back to the effective
        // current (a missing current is never current), or start nothing.
        let current = self.current_project(cx).map(|project| project.id.clone());
        // In its own worktree or in the project root (docs/82 6.1): a pick
        // made for this start, else the project's default.
        let worktree = self.take_worktree_plan(current.as_deref(), switch, cx);
        // A live draft in the target project is the session: reopen its view
        // without touching the wire.
        if let Some(id) = current.clone() {
            match draft_decision(&self.open_state.drafts, &id, |candidate| self.is_live_draft(candidate, cx)) {
                DraftDecision::Reuse(draft_id) => {
                    // A draft serves only the lane it was created on: a muse
                    // draft under a provider pick (or the reverse) is stale,
                    // dropped below, and started fresh on the pick. So is a
                    // draft in the project root when a worktree is wanted,
                    // and the reverse.
                    let wanted = self.new_provider_id(cx);
                    // A parcel waiting for this new session (docs/82 8.2)
                    // rides only this reused draft: key it before the view
                    // lands, so `activate` hands it over here — and only
                    // here.
                    self.browser.update(cx, |browser, _| browser.key_pending_parcels(&draft_id));
                    if wanted.is_some_and(|wanted| self.draft_serves(&draft_id, wanted, worktree.in_worktree(), cx))
                        && self.open_draft(&draft_id, window, cx)
                    {
                        return;
                    }
                    // Named but viewless, or riding the wrong lane: the MRU
                    // never evicts a draft, so a missing view is unreachable
                    // — drop the stale name and start below rather than
                    // strand a retarget.
                    self.open_state.drafts.remove(&id);
                }
                DraftDecision::Start { stale: Some(_) } => {
                    self.open_state.drafts.remove(&id);
                }
                DraftDecision::Start { stale: None } => {}
            }
        }
        // The switcher's pick is not muse: the session opens on the provider
        // lane, and `session/start` to muse never fires for it.
        let Some(wanted) = self.new_provider_id(cx) else {
            crate::baaz_log!("new session refused: no provider `{}` is registered", self.new_provider(cx));
            return;
        };
        // Every provider opens on its lane; Muse too, live — its lane rides
        // the shared child, which the open waits for while it starts. Only
        // a window on the scripted stand-in keeps Muse's local draft.
        if !(wanted.has(provider::ProviderTrait::LocalScriptedDraft) && self.providers.lands_inline()) {
            let root = current
                .as_deref()
                .and_then(|id| self.session_list.read(cx).projects.find_available(id))
                .map(|project| project.root.to_string_lossy().into_owned());
            // A session with no project — on any provider — goes to the
            // default workspace, which is not a project, so it files as
            // unfiled (docs/81 F C1).
            self.ensure_provider_started(wanted, cx);
            // A worktree is made first, off the UI thread; the lane opens
            // in it once it exists (never for an unfiled session).
            let workspace = match (worktree, root) {
                (worktree::WorktreePlan::New, Some(root)) => {
                    self.open_in_new_worktree(wanted, current.clone(), root, switch, cx);
                    return;
                }
                (worktree::WorktreePlan::Reuse(folder), _) => folder,
                (_, root) => root.unwrap_or_else(|| self.unfiled_workspace()),
            };
            self.open_new_on_provider(wanted, current.clone(), workspace, switch, window, cx);
            return;
        }
        // The scripted stand-in (`--no-connect` / `--replay`): no child to
        // start a session on, so the draft opens as a local view. Its id
        // is deterministic per project so captures read the same.
        // Live, with Muse not running yet: bring it up for the next
        // send (docs/50 P1); offline chrome is a no-op here.
        self.ensure_muse_started(cx);
        self.open_local_draft(window, cx);
    }

    #[cfg(feature = "probe")]
    /// `session/start` in `root`, which is never adopted: no project
    /// defaults, no `projects.current` pointer, no `drafts` entry. The
    /// session groups nowhere (`Projects::resolve` finds no adoption for
    /// this root) and shows under Unfiled, exactly like any
    /// session whose folder was never adopted.
    ///
    /// This is [`Self::new_session_in`]'s sibling for a folder that is not,
    /// and is not becoming, a project — the folder panel's target when
    /// "New session" has nowhere current to start (see
    /// [`Self::new_session_in`]'s `None` arm) and [`Self::step_new_in`]'s
    /// headless twin. There is no live-draft reuse here: an unadopted root
    /// has no project id to key a draft on, so every call starts a fresh
    /// session.
    pub(crate) fn new_session_in_root(&mut self, root: &std::path::Path, window: &mut Window, cx: &mut Context<Self>) {
        // Like [`Self::new_session_in`]: a `new` while a switch is pending
        // cancels it and opens normally.
        self.cancel_pending_switch(cx);
        // Same one-session rule as [`Self::new_session_in`]: a switch in
        // flight owns the next session unless a provider switch claimed it.
        if !self.open_state.admit_new() {
            crate::baaz_log!("new: a session is already opening; keeping it");
            self.browser.update(cx, |browser, _| browser.drop_untargeted_parcels());
            return;
        }
        self.new_session_in_root_open(root, window, cx, None);
    }

    #[cfg(feature = "probe")]
    fn new_session_in_root_open(
        &mut self,
        root: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
        switch: Option<u64>,
    ) {
        let root = root.to_path_buf();
        // Same lane branch as `new_session_in`: an unadopted root on a
        // provider pick opens on the provider lane, never as `session/start`.
        let Some(wanted) = self.new_provider_id(cx) else {
            crate::baaz_log!("new session refused: no provider `{}` is registered", self.new_provider(cx));
            return;
        };
        if !(wanted.has(provider::ProviderTrait::LocalScriptedDraft) && self.providers.lands_inline()) {
            self.ensure_provider_started(wanted, cx);
            self.open_new_on_provider(wanted, None, root.to_string_lossy().into_owned(), switch, window, cx);
            return;
        }
        // The scripted stand-in (`--no-connect` / `--replay`): no child to
        // start a session on, so the draft opens as a local view.
        // Live, with Muse not running yet: bring it up for the next
        // send (docs/50 P1); offline chrome is a no-op here.
        self.ensure_muse_started(cx);
        self.open_local_draft(window, cx);
    }

    /// Whether `session_id` still names an unsent draft: a row with zero
    /// turns and no name, or — drafts have no row — a live view whose fold
    /// holds no turns and nobody named. A replayed row is never a draft.
    /// (A turn in flight is checked by the callers that move content.)
    pub(crate) fn is_live_draft(&self, session_id: &str, cx: &gpui::App) -> bool {
        if let Some(entry) = self.session_list.read(cx).sessions.iter().find(|e| e.id == session_id) {
            return entry.turns == 0 && !entry.named && !entry.replayed;
        }
        if self
            .session_list.read(cx).overrides
            .get(session_id)
            .and_then(|m| m.name.clone())
            .is_some_and(|n| !n.trim().is_empty())
        {
            return false;
        }
        let Some(view) = self.find_view(session_id, cx) else { return false };
        view.read(cx).rt(cx).session().is_none_or(|session| session.turns.is_empty())
    }

    /// Whether the draft's view already rides `wanted`'s lane, in a
    /// worktree exactly when `worktree` asks for one: a muse draft serves a
    /// muse pick, and a provider draft serves its own provider's pick.
    /// Anything else (or a view that is gone) is stale — the start below
    /// drops the name and opens fresh on the pick rather than reopening the
    /// wrong lane or the wrong folder.
    fn draft_serves(&self, draft_id: &str, wanted: ProviderId, worktree: bool, cx: &gpui::App) -> bool {
        let Some(view) = self.find_view(draft_id, cx) else { return false };
        // A local Muse draft stood in while Muse was not running; once it
        // runs, the draft cannot send, so a new session starts a real one.
        if is_local_draft(draft_id) && self.providers.shared_ready() {
            return false;
        }
        let rt = view.read(cx).rt(cx);
        rt.provider_kind() == wanted && (is_local_draft(draft_id) || Self::in_worktree(rt.workspace_path()) == worktree)
    }

    /// Reopen a live draft's view: the active one when it is already open,
    /// else the parked one out of the MRU. A retargeted draft in flight
    /// lands in it when it holds nothing. Returns whether the view was
    /// still around to open.
    fn open_draft(&mut self, draft_id: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let moving = self.open_state.pending_draft.take();
        if self.active.as_ref().is_some_and(|view| view.read(cx).rt(cx).session_id == draft_id) {
            if let Some(view) = self.active.clone() {
                Self::land_moving_draft(&view, moving, window, cx);
            }
            return true;
        }
        let Some(view) = self.cache_take(draft_id, window, cx) else {
            self.open_state.pending_draft = moving;
            return false;
        };
        Self::land_moving_draft(&view, moving, window, cx);
        self.activate(view, false, window, cx);
        true
    }

    /// Land a retargeted draft in its new session: moved content wins an
    /// empty composer, and a composer that already holds something keeps it —
    /// the moved text is dropped rather than clobbering what was there.
    fn land_moving_draft(
        view: &Entity<SessionView>,
        moving: Option<Draft>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(draft) = moving else { return };
        view.update(cx, |view, vc| {
            if view.draft_content_empty(vc) {
                view.put_draft(draft, window, vc);
            } else {
                crate::baaz_log!(
                    "draft retarget dropped: the picked project's draft already holds text"
                );
            }
        });
    }

    /// The replay-only stand-in for `session/start`: open the current
    /// project's draft as a local view with no child behind it. Recorded in
    /// `drafts` like a started session, carrying the project's effort and a
    /// retargeted draft when one is in flight.
    fn open_local_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.current_project_id() else {
            crate::baaz_log!("new: no current project; starting nothing");
            self.browser.update(cx, |browser, _| browser.drop_untargeted_parcels());
            return;
        };
        if self.session_list.read(cx).projects.find(&id).is_none() {
            crate::baaz_log!("new: no current project; starting nothing");
            self.browser.update(cx, |browser, _| browser.drop_untargeted_parcels());
            return;
        }
        let moving = self.open_state.pending_draft.take();
        let retarget = moving.is_some();
        let draft_id = format!("{LOCAL_DRAFT_PREFIX}{id}");
        let workspace = self.workspace(cx);
        self.load_menu_sources(std::path::PathBuf::from(workspace.clone()), cx);
        let host = SessionHost {
            provider_id: self.new_provider(cx).to_owned(),
            workspace,
            overlays: self.overlays.clone(),
            capture: self.capture.clone(),
            terminal_host: Some(self.terminal_host(cx)),
        };
        let view = cx.new(|cx| SessionView::new(draft_id.clone(), host, window, cx));
        view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.load_history(cx)));
        // Live, Muse is starting under this draft: the chip and the model
        // menu say so (the promotion replaces the view once it is up).
        let starting = !self.providers.lands_inline()
            && !self.providers.shared_ready()
            && matches!(self.wire, crate::app::Wire::Connecting | crate::app::Wire::Reconnecting);
        view.read(cx).runtime().clone().update(cx, |rt, cx| rt.set_muse_starting(starting, cx));
        // A waiting parcel's session is this draft: key the parcel to it
        // before the view lands, so `activate` hands it over here — and
        // only here.
        self.browser.update(cx, |browser, _| browser.key_pending_parcels(&draft_id));
        self.activate(view, false, window, cx);
        // No row exists for a draft, so the empty state and the later
        // `turn/started` row read the project straight from the store.
        self.set_override(&draft_id, |meta| meta.project = Some(id.clone()), cx);
        let name = self.current_project(cx).map(|p| p.name.clone());
        let effort = self
            .session_list.read(cx).projects
            .picks(self.current_project(cx))
            .effort_for(self.new_provider(cx))
            .as_deref()
            .and_then(projects::parse_effort);
        if let Some(view) = self.active.clone() {
            view.update(cx, |view, cx| {
                view.set_project_name(name);
                // The cached centre reuses a clean view: push the repaint
                // with the name.
                cx.notify();
            });
            if let Some(effort) = effort {
                view.read(cx).runtime().clone().update(cx, |rt, cx| rt.set_initial_effort(Some(effort), cx));
            }
            Self::land_moving_draft(&view, moving, window, cx);
        } else if moving.is_some() {
            self.open_state.pending_draft = moving;
        }
        self.open_state.drafts.insert(id.clone(), draft_id);
        if let Some(view) = self.active.clone() {
            self.sync_new_row(&view, cx);
        }
        crate::baaz_log!(
            "session/start project={} reason={}",
            id,
            if retarget { "retarget" } else { "no-draft" }
        );
        cx.notify();
    }

    /// `session/resume`, then stream the transcript in.
    ///
    /// The target is recorded and shown at once: a cached view reopens
    /// instantly and tops up from its last cursor, otherwise a fresh view
    /// opens on its loading row and pages stream in behind it. Either way a
    /// failed `session/resume` keeps the new view and reports.
    ///
    /// This is the outside-the-sidebar path (palette, `open:` step, boot
    /// `--session`): it arms the one-shot reveal. A sidebar or rail click
    /// reaches [`Self::resume_quiet`] instead and never moves the list.
    pub(crate) fn resume(&mut self, session_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.resume_inner(session_id, false, false, window, cx);
    }

    /// A sidebar or rail click on a session: [`Self::resume`] without the
    /// one-shot reveal. The clicked row is under the cursor, hence painted
    /// inside the viewport by definition, so there is nothing to ensure —
    /// and any stale arm from an earlier outside activation is dropped
    /// rather than served.
    pub(crate) fn resume_quiet(&mut self, session_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.resume_inner(session_id, true, false, window, cx);
    }

    #[cfg(feature = "probe")]
    /// Whether `session_id` names a session this window knows: a sidebar
    /// row, a provider record, a local index entry, or an open or parked
    /// view. What the `open:`/`click:`/`pin:` steps check before acting,
    /// since offline `resume` would open any id as a blank local view.
    pub(crate) fn knows_session(&self, session_id: &str, cx: &gpui::App) -> bool {
        self.session_list.read(cx).sessions.iter().any(|entry| entry.id == session_id)
            || self.session_list.read(cx).provider_sessions.contains_key(session_id)
            || self.session_list.read(cx).index.contains_key(session_id)
            || self.registry.is_parked(session_id)
            || self.active.as_ref().is_some_and(|view| view.read(cx).rt(cx).session_id == session_id)
    }

    /// Open a chain member itself, bypassing the head redirect: the X3a
    /// fallback divider's "earlier turns are in the previous session" link.
    /// A retired source view keeps its read-only banner; this only skips
    /// the resolve, it grants nothing.
    pub(crate) fn resume_source_read_only(
        &mut self,
        source: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resume_inner(source, false, true, window, cx);
    }

    fn resume_inner(
        &mut self,
        session_id: String,
        quiet: bool,
        bypass_chain: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A handoff chain opens as its head: any member id — a sidebar or
        // rail click, the palettes, an `open:` step, boot `--session`, a
        // card or divider link — lands on the head instead. Only the
        // fallback divider's source link bypasses, through
        // `resume_source_read_only`.
        let session_id = if bypass_chain { session_id } else { self.chain_head(&session_id, cx) };
        // A hidden session is never loaded. Hiding is a decision about this
        // window's list, and a list that still opened what it refuses to show
        // would be a list that means nothing. Archived sessions are the same.
        if self.session_list.read(cx).overrides.get(&session_id).is_some_and(|m| m.hidden) && !self.session_list.read(cx).show_hidden {
            return;
        }
        if self.session_list.read(cx).overrides.get(&session_id).is_some_and(|m| m.archived) && !self.session_list.read(cx).show_archived {
            return;
        }
        // B2a-H2b: opening a session while in Browse switches to
        // Sessions first — the palettes, search, the rail, the `open:`
        // step and boot all funnel through here, and a session opened
        // behind Browse would otherwise stay invisible.
        if crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse {
            self.set_mode(crate::layout::Mode::Sessions, window, cx);
        }
        self.adopt_session_project(&session_id, cx);
        // Selecting a session returns from the Skills page (D54).
        self.leave_skills(cx);
        // A click while a provider switch is pending cancels the switch:
        // the old view unlocks with its chip and draft intact, the
        // switch's open lands stale (child shut down, nothing parked), and
        // this resume proceeds as a normal click on the current provider.
        // Clicking the very view the switch was replacing just stays there.
        // Q4b: the same holds for a pending provider reopen (no replace
        // marked): the epoch moves, the loading state drops with the
        // old view's pane restored, and clicking the still-active session
        // stays there without reopening it.
        if self.cancel_pending_switch(cx)
            && self.active.as_ref().is_some_and(|view| view.read(cx).rt(cx).session_id == session_id)
        {
            return;
        }
        // The click's target first: the row highlights on the click's own
        // frame, even when there is no client to open with (a `--replay`
        // capture, the `open:` step's screenshots). `activate` below sets
        // the highlight again on the paths that reach it. Only an outside
        // activation arms the reveal; a quiet click clears any stale arm
        // instead.
        self.open_state.pending_id = Some(session_id.clone());
        if quiet {
            self.sidebar_pane.update(cx, |pane, cx| pane.set_reveal(None, false, cx));
        } else {
            // A fresh arm owns the list again: the next user scroll disarms
            // it.
            let id = session_id.clone();
            self.sidebar_pane.update(cx, |pane, cx| pane.set_reveal(Some(id), true, cx));
        }
        // Live, a session its provider lists (Muse) speaks through its
        // newest list row first: the first open in Baaz of a session made
        // elsewhere writes its record from that row, and every later one
        // refreshes the record's turns and title (docs/70 §3). Scripted
        // chrome has no child to resume from and keeps its local view below.
        if !self.args.offline && self.args.replay.is_none() {
            // A parked view with no record (a provider draft still opening
            // under its request id) just activates: it is no listed
            // session, and nothing must write a record for it.
            if !self.session_list.read(cx).provider_sessions.contains_key(&session_id) {
                if let Some(view) = self.cache_take(&session_id, window, cx) {
                    crate::log::trace_reset();
                    crate::log::trace_mark("cache-hit");
                    self.activate(view, quiet, window, cx);
                    crate::log::trace_arm_first_frame();
                    return;
                }
            }
            self.adopt_listed_record(&session_id, cx);
        }
        // Every session with a record: a parked view just activates — the
        // click switches to the live session (its lane kept following) —
        // and otherwise the stored record reopens over `ResumeSession`,
        // with its history replayed or paged in.
        if let Some(record) = self.session_list.read(cx).provider_sessions.get(&session_id).cloned() {
            // Traced like a Muse switch: `HistoryReady` marks the stored
            // history painted into the reopened lane.
            crate::log::trace_reset();
            crate::log::trace_mark(&format!("resume {} {}", record.provider, session_id.chars().take(8).collect::<String>()));
            crate::log::trace_arm_first_frame();
            if let Some(view) = self.cache_take(&session_id, window, cx) {
                crate::log::trace_mark("cache-hit");
                self.activate(view, quiet, window, cx);
                return;
            }
            crate::log::trace_mark("cache-miss");
            self.reopen_provider(record, window, cx);
            return;
        }
        // Scripted chrome (`--no-connect` / `--replay`) has no child to
        // resume from: the row opens as a local view, so a capture can
        // drive drafts and switching for nothing. Live reconnects never
        // land here — they keep their client-shaped early return below
        // because `args.offline` is false for them.
        if self.args.offline {
            // The row is already open on this view (a `--replay` window
            // whose capture is folded): keep it, so a click on the open
            // row selects without wiping the folded transcript for a
            // blank local view. The highlight above already moved.
            if self.active.as_ref().is_some_and(|view| view.read(cx).rt(cx).session_id == session_id) {
                return;
            }
            // Traced like the live switch below, so the offline perf
            // budget (`scripts/perf-budgets.py`) times a cached switch.
            crate::log::trace_reset();
            crate::log::trace_mark(&format!("resume {}", session_id.chars().take(8).collect::<String>()));
            crate::log::trace_arm_first_frame();
            if let Some(view) = self.cache_take(&session_id, window, cx) {
                crate::log::trace_mark("cache-hit");
                self.activate(view, quiet, window, cx);
            } else {
                self.open(session_id.clone(), quiet, window, cx);
                // An existing session opens loading, never the hero —
                // even with no client behind it.
                if let Some(view) = self.active.clone() {
                    view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.mark_history_loading(cx)));
                    self.attach_handoff_prefix(&view, cx);
                }
            }
        }
    }

    /// Write or refresh the record of a session its provider lists (a Muse
    /// session) from its newest list row — the P4-B migration trigger
    /// (docs/70 §3): *the first time an old Muse session is opened in
    /// Baaz*, never at launch or on list arrival. A session with neither a
    /// record nor a row (boot `--session` before the list lands) is the
    /// listing provider's too — it is the only one whose sessions exist
    /// outside Baaz's records — and gets a record with what is known.
    /// Claude Code and Codex sessions always have their record already.
    fn adopt_listed_record(&mut self, session_id: &str, cx: &mut Context<Self>) {
        let known = self.session_list.read(cx).provider_sessions.get(session_id).map(|record| record.provider.clone());
        let row = self.session_list.read(cx).sessions.iter().find(|entry| entry.id == session_id && !entry.replayed);
        let provider = match (row, known.as_deref()) {
            (Some(row), _) if row.provider.has(provider::ProviderTrait::NativeSessionList) => row.provider,
            (None, None) => match ProviderId::all().into_iter().find(|id| id.has(provider::ProviderTrait::NativeSessionList)) {
                Some(id) => id,
                None => return,
            },
            // A record but no listing row, or another provider's session:
            // nothing newer to read.
            _ => return,
        };
        let listed = self
            .session_list.read(cx).fetch.saved
            .as_deref()
            .and_then(|list| list.sessions.iter().find(|listed| listed.session_id == session_id));
        let index = self.session_list.read(cx).index.get(session_id);
        let title = listed.and_then(|l| l.title.clone()).or_else(|| index.map(|i| i.title.clone()));
        let first_prompt = listed
            .and_then(|l| l.first_user_prompt.clone())
            .or_else(|| index.and_then(|i| i.first_user_prompt.clone()));
        let (workspace, project, turns, created_ms, updated_ms) = match row {
            Some(row) => (
                row.workspace.clone(),
                row.project.clone(),
                if row.turns_unknown { 0 } else { row.turns },
                row.created.timestamp_millis(),
                row.updated.timestamp_millis(),
            ),
            None => {
                let now = crate::usage::now_ms();
                (None, None, 0, now, now)
            }
        };
        let changed = self.session_list.update(cx, |list, _| {
            crate::provider_sessions::upsert_listed(
                &mut list.provider_sessions,
                &mut list.overrides,
                provider.as_str(),
                session_id,
                workspace,
                project,
                title,
                first_prompt,
                turns,
                created_ms,
                updated_ms,
            )
        });
        if changed {
            crate::baaz_log!("record written for {} session {session_id} from its list row", provider.as_str());
            self.persist_provider_sessions(cx);
        }
    }

    /// Whether the session's row records a first prompt: a prompt was
    /// sent (or imported), so it has history somewhere.
    pub(crate) fn first_prompt_sent(&self, session_id: &str, cx: &gpui::App) -> bool {
        self.session_list.read(cx).overrides.get(session_id).is_some_and(|meta| meta.first_prompt.is_some())
    }

    /// Whether a record holds no history anywhere — no settled turn, no
    /// prompt ever sent — so its reopen opens fresh instead. A session its
    /// provider lists counts only when its list row confirms zero turns: a
    /// record written with no row behind it never drops a real session.
    pub(super) fn record_holds_no_history(&self, record: &crate::provider_sessions::ProviderSessionRecord, cx: &gpui::App) -> bool {
        if record.turns > 0 || self.first_prompt_sent(&record.session_id, cx) {
            return false;
        }
        let listed = ProviderId::parse_known(&record.provider).is_some_and(|id| id.has(provider::ProviderTrait::NativeSessionList));
        !listed
            || self
                .session_list.read(cx).sessions
                .iter()
                .any(|entry| entry.id == record.session_id && !entry.turns_unknown && entry.turns == 0)
    }

    /// Whether a reopen on `provider_id` must not register the terminal
    /// bridge first: its shared child refused the resume's bridge once
    /// this run (the hub's memo), so the relay would answer for nothing.
    pub(super) fn resume_bridge_refused(&self, provider_id: ProviderId) -> bool {
        provider_id.has(provider::ProviderTrait::SharedChild) && self.providers.shared().is_some_and(|shared| shared.refuses_resume_bridge())
    }

    /// The root a session of this id runs in: its own row's workspace when
    /// the list knows it, else where the next session would go. Anything
    /// about a session reads this; anything about "where the next session
    /// goes" reads the current project directly.
    pub(crate) fn session_workspace(&self, session_id: &str, cx: &gpui::App) -> String {
        self.session_list.read(cx).sessions
            .iter()
            .find(|e| e.id == session_id)
            .and_then(|e| e.workspace.clone())
            .unwrap_or_else(|| self.workspace(cx))
    }

    /// A session view opened: the current project becomes that session's
    /// project when it has one. Made current and written, so the next boot
    /// and the next ⌘N start where this session is — but never touched:
    /// opening a session must not reorder the groups.
    /// The touch that remains is in [`Self::new_session_in`] (a new session
    /// is itself the freshest thing about its project) and in `adopt_root`,
    /// and both feed only the boot and removal fallbacks
    /// ([`Projects::most_recent_available`](crate::projects::Projects::most_recent_available))
    /// now.
    fn adopt_session_project(&mut self, session_id: &str, cx: &mut Context<Self>) {
        let project = self
            .session_list.read(cx).sessions
            .iter()
            .find(|e| e.id == session_id)
            .and_then(|e| e.project.clone())
            .filter(|id| self.session_list.read(cx).projects.find(id).is_some());
        let Some(id) = project else { return };
        let moved = self.current_project.as_deref() != Some(id.as_str());
        self.session_list.update(cx, |list, _| list.projects.current = Some(id.clone()));
        self.current_project = Some(id);
        projects::write(&self.session_list.read(cx).projects);
        // The project moved under the open Skills page: re-list (D55).
        if moved && self.skills_open(cx) {
            self.relist_skills(cx);
        }
    }

    /// Put a fresh session view in the centre pane now and subscribe to what
    /// it needs help with. The swap is immediate — the view, or its loading
    /// row while the first page is still on the wire — never the old view
    /// held past its switch. `quiet` is the sidebar click (see
    /// [`Self::resume_quiet`): the swap happens, the reveal does not arm.
    pub(in crate::app) fn open(
        &mut self,
        session_id: String,
        quiet: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Scripted chrome's row (`--no-connect` / `--replay`): it opens
        // read-only, with no lane — which is what lets a capture drive
        // drafts and switching. A live reopen goes through
        // `reopen_provider` for every provider (docs/70). Always the
        // provider whose scripted rows are local views
        // ([`provider::ProviderTrait::LocalScriptedDraft`]) — never the
        // new-session default, which read "Codex" or "Claude Code" on a
        // reopened row of that provider.
        let Some(local) = ProviderId::with(provider::ProviderTrait::LocalScriptedDraft) else { return };
        let (provider, workspace) = (local.as_str().to_owned(), self.session_workspace(&session_id, cx));
        self.load_menu_sources(std::path::PathBuf::from(workspace.clone()), cx);
        let overlays = self.overlays.clone();
        let host = SessionHost { provider_id: provider, workspace, overlays, capture: self.capture.clone(), terminal_host: Some(self.terminal_host(cx)) };
        let view = cx.new(|cx| SessionView::new(session_id.clone(), host, window, cx));
        view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.load_history(cx)));
        self.activate(view, quiet, window, cx);
        self.adopt_session_project(&session_id, cx);
        cx.notify();
    }

    /// Refresh the account menu's usage cards: every provider readable
    /// without a model turn is requested. Open lanes peek synchronously
    /// (a busy lane simply keeps its last reading); Muse re-reads the
    /// free wire and Codex runs its read-only probe off-thread, their
    /// rows reading "Refreshing…" in place until each lands. Each
    /// provider is throttled on its own one-minute clock, and skipped
    /// entirely in deterministic mode, where the scripted statuses are
    /// the cards.
    pub(crate) fn refresh_account_usage(&mut self, cx: &mut Context<Self>) {
        if crate::runtime::providers_scripted() {
            return;
        }
        let targets =
            crate::provider_status::usage_refresh_targets(&crate::provider_status::live_statuses());
        let due = crate::provider_status::note_usage_refresh_for(&targets);
        if due.is_empty() {
            return;
        }
        // A remembered pay-as-you-go older than the re-verify window is
        // re-probed here too, not only at boot: a long-running window must
        // not keep telling the person every turn bills when that answer
        // was a one-off read (B7).
        if matches!(self.tier, Some(crate::tier::Tier::PayAsYouGo))
            && !self.tier_probing
            && crate::tier::cached_at(crate::tier::now_secs()).is_none()
        {
            self.probe_tier(false, cx);
        }
        // Muse's quiet re-read, on the menu's terms only: the tier never
        // answered, failed, went dark, or is older than about five
        // minutes. The wire only — never the pty scrape on every open.
        // The tier is the billing-tier provider's reading (Muse's).
        let tier_due = due.iter().copied().find(|id| id.has(provider::ProviderTrait::BillingTier));
        let muse_async = tier_due.is_some()
            && self.providers.shared_ready()
            && crate::tier::muse_menu_refresh_due(self.tier.as_ref());
        let mut lanes: Vec<(ProviderId, provider::UsageReport)> = Vec::new();
        let views = self.registry.live(self.active.as_ref());
        for view in &views {
            let seen = view.read(cx);
            let id = seen.rt(cx).provider_kind();
            if !due.contains(&id) {
                continue;
            }
            if let Some(report) = seen.rt(cx).lane_usage() {
                lanes.push((id, report));
            }
        }
        // The in-memory tier snapshot rides the synchronous record unless
        // the wire re-read below is about to replace it. It carries the
        // remembered answer's own probe time, so re-recording it never
        // re-stamps its age (an unchanged reading writes nothing at all).
        let muse = if tier_due.is_some() && !muse_async {
            self.tier.as_ref().map(|tier| {
                (
                    Some(tier.footer_label()),
                    tier.weekly_fraction().map(|fraction| fraction as f64),
                    crate::tier::cached_probed_at().map(|at| at as i64),
                )
            })
        } else {
            None
        };
        crate::provider_status::record_refreshed_usage(&lanes, muse);
        // The asynchronous reads: their rows read "Refreshing…" until
        // each completion clears its provider and repaints the menu.
        let mut async_ids = Vec::new();
        if let Some(id) = tier_due.filter(|_| muse_async) {
            async_ids.push(id);
        }
        if due.contains(&ProviderId::Codex) {
            async_ids.push(ProviderId::Codex);
        }
        if async_ids.is_empty() {
            return;
        }
        self.set_usage_refreshing(&async_ids, true, cx);
        if muse_async {
            self.refresh_muse_usage(cx);
        }
        if async_ids.contains(&ProviderId::Codex) {
            self.wire_call(
                cx,
                || {
                    // A panic on the background path must never stick the
                    // row on "Refreshing…": the completion below still runs
                    // and clears it (and the 20 s guard bounds a hang).
                    std::panic::catch_unwind(|| {
                        let mut probed = crate::provider_status::Service::with_probes(
                            crate::provider_status::Probes::real(),
                        );
                        probed.probe_one(ProviderId::Codex);
                        probed.status(ProviderId::Codex).usage
                    })
                    .ok()
                    .flatten()
                },
                |this, usage, cx| {
                    // A reading replaces the held one; a probe that read
                    // nothing keeps it — either way the row settles.
                    if let Some(snapshot) = usage {
                        crate::provider_status::record_probed_snapshot(snapshot);
                    }
                    this.set_usage_refreshing(&[ProviderId::Codex], false, cx);
                },
            );
        }
    }

    /// `ids`' usage rows start (`true`) or stop reading "Refreshing…" in
    /// the account menu, which the root draws.
    pub(crate) fn set_usage_refreshing(&mut self, ids: &[ProviderId], refreshing: bool, cx: &mut Context<Self>) {
        if refreshing {
            crate::provider_status::mark_usage_refreshing(ids);
        } else {
            crate::provider_status::clear_usage_refreshing(ids);
        }
        cx.notify();
    }

    /// Forget `session_id`'s view wherever it lives — open or parked. A
    /// provider lane's child is hung up first, so a discarded session leaves
    /// no orphaned `claude` / `codex` process; a muse draft has no child to
    /// hang up. A landed `SwitchProvider` closes the replaced view through
    /// `close_replaced` in the same update instead, so no frame renders
    /// without a session.
    pub(crate) fn close_view(&mut self, session_id: &str, cx: &mut Context<Self>) {
        // A destination that closes before it acknowledged the pack never
        // sends `HandoffPackFailed`: fail its still-Prepared run here, so
        // the links roll back instead of lingering with it.
        self.fail_handoff_for_lost_destination(
            session_id,
            "the destination session closed before it acknowledged the pack".to_owned(),
            cx,
        );
        // Closing a view while a provider switch is pending cancels the
        // switch: the in-flight open lands stale (child shut down, nothing
        // parked or persisted) and the close proceeds normally.
        self.cancel_pending_switch(cx);
        // A provider draft that never landed (opening, or its open failed)
        // takes its provisional names with it.
        let never_landed = self.find_view(session_id, cx).is_some_and(|view| view.read(cx).rt(cx).is_opening_lane());
        // Its folder, when that is a worktree nobody will use: an unsent
        // draft's, or a draft's still opening. A failed open keeps it: Retry
        // opens there again.
        let folder = self
            .find_view(session_id, cx)
            .filter(|view| !view.read(cx).rt(cx).is_opening_lane() || view.read(cx).rt(cx).is_starting())
            .map(|view| view.read(cx).rt(cx).workspace_path().to_owned());
        if never_landed {
            self.forget_provisional(session_id, cx);
        }
        // The next `activate` re-points the event subscription at the new
        // view; the dropped view's subscription fires nothing after this,
        // so this only hangs up children and forgets handles — never while
        // an event for the closed view is still dispatching through them.
        if self.active.clone().is_some_and(|view| view.read(cx).rt(cx).session_id == session_id) {
            if let Some(view) = self.active.take() {
                view.read(cx).runtime().clone().update(cx, |rt, _| rt.shutdown_lane());
            }
            self.open_state.pending_id = None;
        }
        self.cache_drop(session_id, cx);
        // A provider draft that never sent leaves no transcript anywhere —
        // no live turns, no replayed ones, not even a first prompt — so its
        // record leaves with its view instead of lingering as a row the
        // empty filter hides forever. Anything with history keeps its
        // record: only the view is forgotten.
        let unsent_draft = self
            .session_list.read(cx).provider_sessions
            .get(session_id)
            .is_some_and(|record| record.turns == 0 && !self.first_prompt_sent(session_id, cx));
        // Its worktree goes with it, when nothing was changed there.
        if unsent_draft || never_landed {
            if let Some(folder) = folder {
                self.release_unused_worktree(&folder, cx);
            }
        }
        if unsent_draft {
            self.delete_provider_session(session_id, cx);
            return;
        }
        cx.notify();
    }

    /// Make `view` the centre pane now: park the outgoing view in the MRU,
    /// point the event subscription at the new one, refresh its context, and
    /// run anything scripted. The frame after this draws the new view.
    ///
    /// Every outside activation path (⌘N, a group `+`, the palette, the
    /// `open:` step, fork, boot `--session`) comes through here with
    /// `quiet == false`, so this is where the sidebar's one-shot reveal is
    /// armed: the next prepaint scrolls the least distance that brings the
    /// row (or, when its group is closed or folded past the cut, the group)
    /// into view, then consumes the flag (`scrollIntoView({ block:
    /// "nearest" })` semantics). A sidebar or rail click
    /// comes through here with `quiet == true` and never arms: the clicked
    /// row is under the cursor, hence visible, and any stale arm is dropped.
    ///
    /// Every session switch lands here — resume, reopen, draft, local
    /// draft, provider lane, fork, handoff destination — so this is where
    /// the Settings page exits for navigation (B11b): the destination owns
    /// the next route, and nothing is restored; the composer's focus is
    /// armed by this same swap below.
    pub(crate) fn activate(
        &mut self,
        view: Entity<SessionView>,
        quiet: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Q4c: every landing invalidates any in-flight provider open. A
        // new-session or fork open (`open_on_provider`,
        // `open_forked_on_provider`) and a retry never set `pending_id`,
        // so `cancel_pending_switch` never sees them — only the landing
        // itself can retire them. The late `finish_provider_open` then
        // lands stale (its child is shut down, nothing is parked or
        // persisted) and never steals the centre. The provider open's own
        // landing passes through here too, after its epoch check already
        // admitted it, so the bump retires nothing of its own. Whatever
        // switch the scripts were waiting for has landed too: session verbs
        // run against this view from here on. And a landed session stands
        // any inline open failure down (Z4): the real view replaces the
        // failure state through this same path.
        self.open_state.land();
        self.exit_settings_for_navigation(cx);
        // Opening a session leaves the Skills page (D54, V1): "New session"
        // from the sidebar or ⌘N activates through here on every path
        // (draft reuse, local draft, provider lane), so the page never
        // swallows the new session the way it did before. Selecting a
        // session closes it earlier in `resume_inner`; closing it here too
        // is idempotent there and covers every other opener.
        self.leave_skills(cx);
        // The UI points at what is open: the row highlight and the header
        // label read this, never the view, so every swap refreshes it here
        // rather than at each call site.
        let session_id = view.read(cx).rt(cx).session_id.clone();
        self.open_state.pending_id = Some(session_id.clone());
        if quiet {
            self.sidebar_pane.update(cx, |pane, cx| pane.set_reveal(None, false, cx));
        } else {
            // The sidebar's one-shot reveal arms on the same swap — and a
            // fresh arm owns the list again: the next user scroll disarms it.
            let id = session_id.clone();
            self.sidebar_pane.update(cx, |pane, cx| pane.set_reveal(Some(id), true, cx));
        }
        let project_name = self.project_name_for(&session_id, cx);
        view.update(cx, |view, _| view.set_project_name(project_name));
        self.sync_new_row(&view, cx);
        // Y2b: a landed switch closes the replaced view in this same update,
        // so no frame renders without a session. Otherwise park as usual.
        // Y2b2: only the current switch epoch may close what it replaced —
        // a view that lands after a cancel or a superseding switch parks
        // like any other view instead of closing one it never replaced.
        let replaced = self
            .open_state
            .current_replace()
            .is_some_and(|old_id| self.close_replaced(&old_id, &view, window, cx));
        // The old view was already gone (or the same entity reopened), or
        // the marker was a stale switch's: drop it without parking anything
        // extra.
        self.open_state.finish_replace();
        if !replaced {
            self.park_active(cx);
        }
        // One subscription per live runtime and per live view: the session
        // being activated plus every parked one the MRU still holds, with a
        // view or without. A parked runtime keeps its child and drain task,
        // so its turn and approval events still fire — with only the active
        // view subscribed they reached no one and the parked row froze
        // (B3b). Rebuilt from the live sessions on every activation, so a
        // dropped one's subscription leaves with it and a re-activated one
        // is never subscribed twice. Parked transcripts never re-render
        // through these: the row-affecting arms read the parked fold
        // without touching its UI.
        self.registry.resubscribe(Some(&view), cx);
        let titles: HashMap<String, String> =
            self.session_list.read(cx).sessions.iter().map(|entry| (entry.id.clone(), entry.label.clone())).collect();
        if view.read(cx).rt(cx).provider_kind().has(provider::ProviderTrait::BillingTier) {
            self.tier_probe_on_muse_lane(cx);
        }
        let tier_banner = self.tier_banner();
        view.update(cx, |view, cx| {
            view.set_context(titles);
            view.set_at_rest(self.still());
            view.runtime().clone().update(cx, |rt, cx| rt.set_tier_banner(tier_banner, cx));
        });
        // The swap must repaint the centre even when every push above was
        // `set` without `notify`: a parked view reuses its retained subtree
        // unless it is dirty.
        view.update(cx, |_, cx| cx.notify());
        self.overlays.update(cx, |o, cx| { o.close(crate::overlay_stack::LayerKind::PageMenu, cx); });
        self.active = Some(view.clone());
        // Another session working in this folder: one warning (docs/82 6.2).
        self.warn_if_folder_busy(&view, cx);
        // Reopening a session that stands at its end reads it (task 3.1);
        // one still loading reads on its `HistoryReady`.
        if view.read(cx).at_transcript_end() && !view.read(cx).rt(cx).history_loading() {
            let session_id = view.read(cx).rt(cx).session_id.clone();
            self.mark_read(&session_id, cx);
        }
        self.arm_composer_focus(cx);
        // "Ask Muse to write one" (A5): the fresh session opens with the
        // `/create-skill ` draft and the composer focused. Never sent. The
        // arm survives a non-muse activation untouched — the draft is a muse
        // skill prompt, so it waits for a muse session.
        if view.read(cx).rt(cx).provider_kind().has(provider::ProviderTrait::ManagedSkills) && self.skills_page.update(cx, |page, _| page.take_create_draft()) {
            let draft = crate::skills_page::create_skill_draft().to_owned();
            view.update(cx, |view, cx| {
                view.set_draft(draft, window, cx);
                view.focus_composer(window, cx);
            });
        }
        // What a page sent this session while its view was away (docs/82
        // 8.2: a picked screenshot, notes, a hand-off) lands now — keyed by
        // its id, never by `self.active` — as a draft only, never sent. An
        // unrelated landing (or one while an async open is still in flight,
        // with no key yet) leaves it waiting; an early return in
        // `new_session_in` dropped a new session's already.
        self.land_parcels(&session_id, &view, window, cx);
        #[cfg(feature = "probe")]
        self.send_scripted(window, cx);
        // No `maybe_run_steps` here: activations fire on every swap,
        // including swaps the script itself causes, and draining the list
        // from the swap raced the boot it was meant to follow. The settle
        // this update queues ([`Harness::settle`]) runs the script once it
        // is ready.
        // The window wears the session's title, and the first session the
        // shell shows warms the Browser pane off the frame (once).
        self.sync_window_title(window, cx);
        self.maybe_idle_prewarm(cx);
        // Z2: the pane belongs to the session now showing — restore it
        // without animating, so every activation path (click, resume,
        // reopen, provider reopen, launch restore) lands on the session's
        // own pane. A session with no stored state (and every brand-new
        // one) shows the pane closed.
        self.restore_right_for_session(&session_id, cx);
        cx.notify();
    }

    /// Park the outgoing view in the MRU: its event subscription drops with
    /// `view_subscriptions`, while its fold, scroll position and draft ride along
    /// in the entity. Hidden, archived and replayed views are never cached.
    /// One subscription per live view ([`SessionRegistry::resubscribe`]):
    /// after a failed open, so a parked turn finishing behind the failure
    /// view still reaches its row (docs/81 A F17), and when a browser chat
    /// is set.
    ///
    /// [`SessionRegistry::resubscribe`]: crate::app::registry::SessionRegistry::resubscribe
    pub(crate) fn resubscribe_live_views(&mut self, cx: &mut Context<Self>) {
        let active = self.active.clone();
        self.registry.resubscribe(active.as_ref(), cx);
    }

    pub(super) fn park_active(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.active.take() else { return };
        let session_id = view.read(cx).rt(cx).session_id.clone();
        // A replayed capture parks only in a scripted window (`--replay`,
        // offline): there it is the session, and switching away and back
        // must find its transcript rather than a blank local view (the
        // offline switch budget in `scripts/perf-budgets.py` times exactly
        // that). A live window never holds a replay worth keeping.
        let cacheable = (!view.read(cx).rt(cx).is_replay() || self.args.offline)
            && !self.session_list.read(cx).overrides.get(&session_id).is_some_and(|m| m.hidden)
            && !self.session_list.read(cx).overrides.get(&session_id).is_some_and(|m| m.archived);
        if cacheable {
            self.registry.park(session_id, view, cx);
            // Views named in `drafts` are never evicted from the MRU: the
            // draft outlives whatever else was parked since. Past the cap a
            // session with work in hand (`must_stay_alive`) — one still
            // starting (a provider draft whose child has not answered holds
            // the sends queued meanwhile), or one with a turn running or
            // the person's answer pending — loses only its view: its
            // runtime runs on, and shutting it would kill the child
            // mid-turn (docs/81 A F1, docs/82 4.8). An idle one goes whole.
            let drafts: std::collections::HashSet<String> = self.open_state.drafts.values().cloned().collect();
            let evicted = self.registry.evict_idle(&drafts, SESSION_CACHE_LIMIT, cx);
            for (id, runtime) in evicted {
                if runtime.read(cx).is_opening_lane() {
                    self.forget_provisional(&id, cx);
                }
            }
        }
    }

    /// Take a parked session back out of the MRU, as a view: its own when
    /// the cap kept it, else a new one built over its runtime — the turn
    /// it ran meanwhile already folded there (docs/82 4.8).
    pub(super) fn cache_take(&mut self, session_id: &str, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<SessionView>> {
        let (runtime, view) = self.registry.take(session_id)?;
        if let Some(view) = view {
            return Some(view);
        }
        let (provider_id, workspace) = {
            let rt = runtime.read(cx);
            (rt.provider_kind().as_str().to_owned(), rt.workspace_path().to_owned())
        };
        let host = SessionHost {
            provider_id,
            workspace,
            overlays: self.overlays.clone(),
            capture: self.capture.clone(),
            terminal_host: Some(self.terminal_host(cx)),
        };
        Some(cx.new(|cx| SessionView::over(runtime, host, window, cx)))
    }

    /// Take a parked session out of the MRU and shut its lane down: a close
    /// or a swap that replaces it.
    pub(super) fn cache_drop(&mut self, session_id: &str, cx: &mut Context<Self>) -> Option<Entity<SessionRuntime>> {
        let (runtime, _) = self.registry.take(session_id)?;
        runtime.update(cx, |rt, _| rt.shutdown_lane());
        Some(runtime)
    }

    #[cfg(feature = "probe")]
    /// Whether any session this window has open — the active one, or one
    /// parked in the MRU (with its view or without) — has a turn in flight
    /// ([`SessionRuntime::is_sending`]: submitted and no `turn/started` yet, or
    /// the server says it runs).
    ///
    /// What a `--screenshot` run checks, bounded, before it quits
    /// ([`crate::shot::capture_and_quit`]): quitting under a running turn
    /// kills its `muse` child and orphans it. Every runtime keeps folding
    /// its own lane's events, parked or not, so this reads each session's
    /// own state.
    pub(crate) fn any_turn_running(&self, cx: &App) -> bool {
        !self.registry.iter_running(self.active.as_ref(), cx).is_empty()
    }

    /// Store a pick as the one every new session starts with, in any
    /// project, and persist (owner 2026-10-21: "Last pick, everywhere").
    /// It needs no session row, so a pick made while a provider draft is
    /// still opening counts too (it used to be dropped: the draft had no
    /// row to resolve a project from).
    pub(super) fn note_pick(&mut self, edit: impl FnOnce(&mut crate::projects::ProjectDefaults), cx: &mut gpui::App) {
        self.session_list.update(cx, |list, _| edit(&mut list.projects.last_picks));
        projects::write(&self.session_list.read(cx).projects);
    }
    /// The view for `session_id`: the active one, else a parked one.
    pub(crate) fn find_view(&self, session_id: &str, cx: &gpui::App) -> Option<Entity<SessionView>> {
        self.registry.find(self.active.as_ref(), session_id, cx)
    }
}
