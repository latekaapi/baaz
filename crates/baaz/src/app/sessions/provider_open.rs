//! Opening a session on a provider lane (Claude Code, Codex, Muse on the
//! seam): the background `OpenSession`, the draft drawn on the click and
//! its landing, failure, timeout and retry, the provisional-id rekey, and
//! reopening or forking a stored provider session.
//!
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;

/// How long a provider draft drawn on the click waits for its child to
/// answer `OpenSession` before its view fails, visibly. The handshakes
/// measured 7 ms (Claude Code) to 1.7 s (Codex); this is for a child that
/// hangs.
const PROVIDER_OPEN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Everything one provider `OpenSession` admitted, carried from the
/// background turn that connected it to the UI thread that opens its view.
struct ProviderOpen {
    provider_id: ProviderId,
    provider: provider::Provider,
    events: futures::channel::mpsc::UnboundedReceiver<provider::ProviderEvent>,
    session_id: String,
    project: Option<String>,
    workspace: String,
    /// The provider's own display title from the ack, when it supplied
    /// one: the label ladder's fallback under a generated title.
    title: Option<String>,
    /// Which open this is ([`OpenState::next_open`]): `finish`
    /// lands only the newest ask, so a late-finishing earlier open never
    /// steals focus or the send.
    epoch: u64,
    /// The resume was refused because another window holds the session
    /// ([`provider::RefusalHint::HeldElsewhere`]): the lane still lands,
    /// read-only under the lease notice.
    lease_held: bool,
    /// The session's history from Baaz's own transcript store, when it held
    /// a fresh copy (docs/85 11.1): the view shows these, and the provider
    /// resumed without sending history.
    stored_turns: Option<Vec<aui_protocol::Turn>>,
    /// The model a fresh open started its thread on, for a provider whose
    /// thread takes its model at its start
    /// ([`provider::ProviderTrait::ModelAtThreadStart`]): the landing notes
    /// it on the runtime, so a different pick before the first turn
    /// restarts the session rather than switching it. `None` for every
    /// resume and fork, and for every other provider.
    fresh_thread_model: Option<Option<String>>,
}

/// The work of every fresh provider open: spawn the child through the
/// factory and ask it for a session with `OpenSession`, carrying
/// `request_id` (Claude Code's `--session-id`; Codex mints its own thread
/// id). Blocks unless the factory lands inline, so it runs through
/// [`Harness::run_open`].
#[allow(clippy::too_many_arguments)]
fn provider_open_work(
    factory: crate::providers::Registry,
    provider_id: ProviderId,
    request_id: String,
    workspace_bg: String,
    extras: OpenExtras,
    project: Option<String>,
    workspace: String,
    epoch: u64,
    cancelled: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<ProviderOpen, crate::app::OpenFailed> {
    let provider = factory.open(provider_id)?;
    let OpenExtras { model_provider, mode, model } = extras;
    let fresh_thread_model = provider_id.has(provider::ProviderTrait::ModelAtThreadStart).then(|| model.clone());
    #[cfg(not(all(test, feature = "probe")))]
    let (provider, events) = conn::gate(provider);
    // `model` is `None` unless the provider's thread takes its model at
    // its start ([`thread_start_model`]): there it is the model the
    // session will use, so its first turn is no model switch. Elsewhere the
    // chip's display seed (env, `claude` settings, the last reported model)
    // never reaches the child — no `--model` flag without a pick — so the
    // CLI resolves its own default exactly as before.
    // A session in one of Baaz's worktrees starts its child with the
    // worktree's variables (docs/82 6.1), so the agent's own commands see
    // its port block.
    let env = baaz_core::worktrees::env_for_folder(&crate::store::support_dir(), std::path::Path::new(&workspace_bg));
    let ack = provider
        .send(provider::Command::OpenSession {
            request_id,
            workspace: Some(workspace_bg),
            model,
            model_provider,
            mode,
            env,
        })
        // The child is about to go with the provider: its stderr travels
        // with the refusal, for the failure view's well (docs/71 §3).
        .map_err(|error| crate::app::OpenFailed { error, stderr: provider.stderr_tail() })?;
    // Tests drain after the send, synchronously on the test
    // executor: no forwarding thread ever wakes the lane task.
    #[cfg(feature = "probe")]
    #[cfg(test)]
    let (mut provider, events) = conn::gate_sync(provider);
    #[cfg(not(all(test, feature = "probe")))]
    let mut provider = provider;
    // The app quit while this open was on the wire: its child would
    // outlive every view, so it is hung up here, on this thread.
    if cancelled.is_some_and(|cancelled| cancelled.load(std::sync::atomic::Ordering::SeqCst)) {
        provider.shutdown();
        return Err(provider::ProviderError::Unavailable { reason: "Baaz is quitting".into() }.into());
    }
    match ack {
        provider::Ack::Session { session_id, title, .. } => Ok(ProviderOpen {
            provider_id,
            provider,
            events,
            session_id,
            project,
            workspace,
            title,
            epoch,
            lease_held: false,
            stored_turns: None,
            fresh_thread_model,
        }),
        other => Err(provider::ProviderError::Rejected {
            reason: format!("OpenSession answered {other:?} instead of a session"),
        }
        .into()),
    }
}

/// What one `OpenSession` carries beyond its workspace, per the provider's
/// registry properties ([`Harness::open_extras`]): the model vendor its
/// requests route to, and the approval mode it starts in — `None` when
/// the host's default posture should stand.
pub(crate) struct OpenExtras {
    model_provider: Option<String>,
    pub(crate) mode: Option<aui_protocol::PermissionMode>,
    /// The model the thread starts on: set by the caller through
    /// [`thread_start_model`], `None` from [`Harness::open_extras`].
    model: Option<String>,
}

/// The model an open on `provider_id` names at its start: `model` when the
/// provider's thread takes its model there
/// ([`provider::ProviderTrait::ModelAtThreadStart`] — a later first-turn
/// pick would be a switch, paid for in every later request), else `None`,
/// so the CLI resolves its own default.
fn thread_start_model(provider_id: ProviderId, model: Option<String>) -> Option<String> {
    model.filter(|model| !model.is_empty() && provider_id.has(provider::ProviderTrait::ModelAtThreadStart))
}

/// Whether a finished turn's model report seeds the next fresh Claude
/// Code session's display-only chip: Claude Code reports only, and never
/// an internal Haiku sub-call's (see
/// [`crate::providers::claude_code_report_matches_session`]) — persisting
/// that would paint every future fresh chip with a model nobody picked.
/// The `ProviderTurnFinished` arm persists through here, so this is the
/// call site's whole seed decision in pure form.
pub(crate) fn should_seed_claude_code_model(
    provider_kind: ProviderId,
    report: &str,
    known: &str,
) -> bool {
    provider_kind == ProviderId::ClaudeCode
        && crate::providers::claude_code_report_matches_session(report, known)
}

impl Harness {
    /// The approval mode a provider session opens (or reopens) in: the
    /// command line's `--approval-mode` first, then the project's last
    /// chip pick (`ModeSelected` writes it), else the seam's default. Always
    /// a mode, and always sent: the adapter starts the child in it and
    /// reports it back, so the chip shows what was asked for until the
    /// child says otherwise — never a default nobody sent.
    /// The model a new session on `provider_id` in `project` asks for: the
    /// last one picked for that provider there. A stored Muse default rides
    /// only while the cached Muse catalog still names it — a foreign pick
    /// kept through migration would fail the turn, so the server's default
    /// stands instead; no other provider's default ever meets Muse's catalog.
    pub(crate) fn start_model(&self, provider_id: ProviderId, project: Option<&str>, cx: &gpui::App) -> Option<String> {
        let picks = self.session_list.read(cx).projects.picks(project.and_then(|id| self.session_list.read(cx).projects.find(id)));
        let remembered = picks.model_for(provider_id.as_str());
        if provider_id.has(provider::ProviderTrait::CatalogFlagsSessionModel) {
            return projects::muse_start_model(remembered, &self.muse_catalog_ids(cx));
        }
        remembered
    }

    /// What an open on `provider_id` carries beyond its workspace
    /// ([`OpenExtras`]). A provider that routes a model vendor through the
    /// open ([`provider::ProviderTrait::RoutesModelVendor`]) names it — the
    /// new-session pick's own spelling when it is this provider (`echo` in
    /// a scripted run), else the provider's id — and starts in the host's
    /// default posture unless a mode was picked; every other provider
    /// carries `mode`, the mode the chip stands on.
    fn open_extras(
        &self,
        provider_id: ProviderId,
        mode: aui_protocol::PermissionMode,
        workspace: Option<&str>,
        project: Option<&str>, cx: &gpui::App) -> OpenExtras {
        if !provider_id.has(provider::ProviderTrait::RoutesModelVendor) {
            return OpenExtras { model_provider: None, mode: Some(mode), model: None };
        }
        let model_provider = if self.new_provider_id(cx) == Some(provider_id) {
            self.new_provider(cx).to_owned()
        } else {
            provider_id.as_str().to_owned()
        };
        let mode = self.args.approval_mode.or_else(|| {
            let list = self.session_list.read(cx);
            list.projects.picks(list.projects.resolve(workspace, project)).mode_for(provider_id.as_str())
        });
        OpenExtras { model_provider: Some(model_provider), mode, model: None }
    }

    /// The mode a new `provider_id` session opens in: the flag, else that
    /// provider's stored mode for the project (docs/82 5.5: per project
    /// per provider), else the seam's default.
    pub(in crate::app) fn provider_open_mode(
        &self,
        provider_id: ProviderId,
        workspace: Option<&str>,
        project: Option<&str>,
        cx: &gpui::App,
    ) -> aui_protocol::PermissionMode {
        self.args
            .approval_mode
            .or_else(|| {
                let list = self.session_list.read(cx);
                list.projects.picks(list.projects.resolve(workspace, project)).mode_for(provider_id.as_str())
            })
            .unwrap_or_default()
    }

    /// Open a new Claude Code / Codex session: connect and `OpenSession` on
    /// the background executor (both block), then construct the lane view and
    /// make it the visible session. What `new_session_in` calls when the
    /// switcher's pick is not muse.
    ///
    /// A failure lands the inline open-failure state (see
    /// [`Self::fail_provider_open`]) — and opens nothing, never a silent
    /// muse fallback. A failed provider SWITCH still dialogs (Y2b).
    pub(crate) fn open_on_provider(
        &mut self,
        provider_id: ProviderId,
        project: Option<String>,
        workspace: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.load_menu_sources(std::path::PathBuf::from(workspace.clone()), cx);
        // The switch lands on the round-trip below, like the muse path: the
        // session verbs wait for it instead of acting on the session that is
        // still open. Whichever open the user asked for last wins: this
        // ask's epoch travels into the background work, and
        // `finish_provider_open` drops any finish that is no longer current.
        let epoch = self.open_state.begin_open();
        // The window's factory (on `--no-connect` / `--replay`, the scripted
        // stand-in: what lets `new;setprovider:claude-code` end on a
        // provider lane offline). A disabled provider opens over the
        // stand-in too, plus the quiet banner — readable, resumable, and
        // sending nothing anywhere.
        let factory = self.lane_factory(provider_id);
        let inline = factory.lands_inline();
        let workspace_bg = workspace.clone();
        let mode = self.provider_open_mode(provider_id, Some(&workspace), project.as_deref(), cx);
        // The terminal relay's route: the request id is the session id the
        // bridge will answer for (Claude Code takes it as `--session-id`;
        // Codex's bridge answers for it until the minted thread id lands) —
        // so it is registered with the service BEFORE the open runs.
        let request_id = provider::new_request_id();
        self.register_terminal_session(
            &request_id,
            std::path::PathBuf::from(workspace.clone()),
            provider_id.as_str(),
            cx,
        );
        // What Retry re-runs when this fresh open fails: cloned up front,
        // because `work` below moves the originals into the background.
        let retry_new =
            ProviderOpenRetry::OpenNew { project: project.clone(), workspace: workspace.clone(), draft: String::new() };
        // The thread id replaces this request id on the view once the lane
        // lands (Codex mints one server-side): carried into the finish below
        // so the relay keeps routing the bridge's `--session <request_id>`
        // calls at the lane's browser and tabs.
        let alias_from = request_id.clone();
        let mut extras = self.open_extras(provider_id, mode, Some(&workspace), project.as_deref(), cx);
        extras.model = thread_start_model(provider_id, self.start_model(provider_id, project.as_deref(), cx));
        let cancelled = std::sync::Arc::clone(&self.open_state.provider_opens_cancelled);
        let work = move || {
            provider_open_work(factory, provider_id, request_id, workspace_bg, extras, project, workspace, epoch, Some(cancelled))
        };
        self.run_open(inline, work, move |this, result, window, cx| match result {
            Ok(open) => {
                this.terminal_service.alias_session(&alias_from, &open.session_id, cx);
                this.finish_provider_open(open, window, cx);
            }
            Err(error) => {
                // A stale open's failure belongs to nobody: it must not
                // clear the current switch, unlock its view, or dialog.
                if !this.open_state.is_current_open(epoch) {
                    crate::baaz_log!(
                        "provider open failed for a stale ask ({}): {error}",
                        provider_id.label()
                    );
                    this.browser.update(cx, |browser, _| browser.drop_untargeted_parcels());
                    return;
                }
                // No switch is coming: release the session verbs waiting on
                // it, like the failed `session/start` arm does.
                crate::baaz_log!("provider open failed ({}): {error}", provider_id.label());
                this.fail_provider_open(provider_id, error, retry_new, None, window, cx);
            }
        }, window, cx);
    }

    /// docs/85 14.2: open a worker's session in the background — the
    /// lane connects and opens like any new session (on `model`, in `mode`,
    /// in `workspace`), gets its record and sidebar row, and is parked live
    /// in the registry, never activated: what the person is looking at stays
    /// on screen. `then` hears the session id (or why it failed).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open_worker(
        &mut self,
        provider_id: ProviderId,
        model: Option<String>,
        mode: aui_protocol::PermissionMode,
        project: Option<String>,
        workspace: String,
        then: impl FnOnce(&mut Self, Result<String, String>, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !crate::provider_status::provider_enabled(provider_id) {
            then(self, Err(format!("{} is switched off in Settings", provider_id.label())), cx);
            return;
        }
        let factory = self.providers.clone();
        let inline = factory.lands_inline();
        let request_id = provider::new_request_id();
        self.register_terminal_session(&request_id, std::path::PathBuf::from(workspace.clone()), provider_id.as_str(), cx);
        let mut extras = self.open_extras(provider_id, mode, Some(&workspace), project.as_deref(), cx);
        extras.mode = Some(mode);
        extras.model = thread_start_model(provider_id, model.clone());
        let alias_from = request_id.clone();
        let (workspace_bg, model_bg) = (workspace.clone(), model.clone());
        let work = move || {
            let open = provider_open_work(factory, provider_id, request_id, workspace_bg, extras, project, workspace, 0, None);
            (open, model_bg)
        };
        self.run_open(inline, work, move |this, (result, model), window, cx| match result {
            Ok(open) => {
                this.terminal_service.alias_session(&alias_from, &open.session_id, cx);
                let id = this.finish_worker_open(open, model, window, cx);
                then(this, Ok(id), cx);
            }
            Err(error) => then(this, Err(error.error.to_string()), cx),
        }, window, cx);
    }

    /// docs/85 14.1: resume a worker's session after a restart, in the
    /// background like [`Self::open_worker`] — the provider's own files are
    /// the authority (rule 18), and nothing is activated. `then` hears the
    /// session id, or why it could not resume.
    pub(crate) fn reopen_worker(
        &mut self,
        session_id: &str,
        mode: aui_protocol::PermissionMode,
        then: impl FnOnce(&mut Self, Result<String, String>, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let record = self.session_list.read(cx).provider_sessions.get(session_id).cloned();
        let Some(record) = record else {
            then(self, Err(format!("no record of session {session_id}")), cx);
            return;
        };
        let Some(provider_id) = ProviderId::parse_known(&record.provider) else {
            then(self, Err(format!("unknown provider {}", record.provider)), cx);
            return;
        };
        let factory = self.lane_factory(provider_id);
        let inline = factory.lands_inline();
        self.ensure_provider_started(provider_id, cx);
        let workspace = record.workspace.clone().unwrap_or_else(|| self.workspace(cx));
        self.register_terminal_session(&record.session_id, std::path::PathBuf::from(workspace.clone()), record.provider.as_str(), cx);
        let project = self.session_list.read(cx).overrides.get(&record.session_id).and_then(|meta| meta.project.clone());
        let work = move || -> Result<ProviderOpen, crate::app::OpenFailed> {
            let provider = factory.open(provider_id)?;
            #[cfg(not(all(test, feature = "probe")))]
            let (provider, events) = conn::gate(provider);
            let env = baaz_core::worktrees::env_for_folder(&crate::store::support_dir(), std::path::Path::new(&workspace));
            let session_id = crate::provider_sessions::send_resume(&provider, &record, &provider::new_request_id(), Some(mode), env, false)
                .map_err(|error| crate::app::OpenFailed { error, stderr: provider.stderr_tail() })?;
            #[cfg(feature = "probe")]
            #[cfg(test)]
            let (provider, events) = conn::gate_sync(provider);
            Ok(ProviderOpen { provider_id, provider, events, session_id, project, workspace, title: None, epoch: 0, lease_held: false, stored_turns: None, fresh_thread_model: None })
        };
        self.run_open(inline, work, move |this, result, window, cx| match result {
            Ok(open) => {
                let id = this.finish_worker_open(open, None, window, cx);
                then(this, Ok(id), cx);
            }
            Err(error) => then(this, Err(error.error.to_string()), cx),
        }, window, cx);
    }

    /// Land a worker's lane: its view, record and row, parked live.
    fn finish_worker_open(&mut self, open: ProviderOpen, model: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> String {
        let ProviderOpen { provider_id, provider, events, session_id, project, workspace, title, fresh_thread_model, .. } = open;
        self.register_terminal_session(&session_id, std::path::PathBuf::from(workspace.clone()), provider_id.as_str(), cx);
        let host = SessionHost {
            provider_id: provider_id.as_str().to_owned(),
            workspace: workspace.clone(),
            overlays: self.overlays.clone(),
            capture: self.capture.clone(),
            terminal_host: Some(self.terminal_host(cx)),
        };
        let view = cx.new(|cx| SessionView::new_on_provider(session_id.clone(), provider, events, host, window, cx));
        crate::baaz_log!("worker lane open provider={} session={session_id} workspace={workspace}", provider_id.as_str());
        // A writer's worktree is tied to its worker here too (the record
        // links it, as for any session).
        self.record_provider_open(provider_id, &session_id, project, workspace, title, cx);
        if let Some(started_on) = fresh_thread_model {
            view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, _| rt.note_fresh_thread(started_on)));
        }
        if let Some(model) = model.filter(|m| !m.is_empty()) {
            view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.set_model(&model, cx)));
        }
        self.registry.park(session_id.clone(), view, cx);
        self.registry.resubscribe(self.active.as_ref(), cx);
        self.push_branch_counts(cx);
        // Nothing on screen changes: its row arrives through the list.
        session_id
    }

    /// The factory an open on `provider_id` runs through: the window's, or
    /// — for a provider switched off in Settings — the scripted stand-in,
    /// with the quiet banner noted for the landing view.
    fn lane_factory(&mut self, provider_id: ProviderId) -> crate::providers::Registry {
        if crate::provider_status::provider_enabled(provider_id) {
            return self.providers.clone();
        }
        self.open_state.pending_disabled_notice = Some(provider_id);
        self.providers.stand_in()
    }

    /// Run an open's `work` and land its result in `then`. Off the UI
    /// thread (`wire_call_in`): every open spawns or waits for a child and
    /// blocks. Inline, in this update, when the factory
    /// [lands inline](crate::providers::Registry::lands_inline): the
    /// scripted stand-in never blocks, and a scripted open has always landed
    /// in the same update as its click — the same work, the same landing.
    fn run_open<T, W, F>(&mut self, inline: bool, work: W, then: F, window: &mut Window, cx: &mut Context<Self>)
    where
        T: Send + 'static,
        W: FnOnce() -> T + Send + 'static,
        F: FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
    {
        if inline {
            let result = work();
            then(self, result, window, cx);
        } else {
            self.wire_call_in(cx, work, then);
        }
    }

    /// Land a connected provider session: construct its lane view and
    /// register it everywhere [`Self::open`] registers muse views — the
    /// drafts map, the project override, the local record with its sidebar
    /// row, and the centre pane with the composer focused — so it becomes
    /// the visible session.
    fn finish_provider_open(&mut self, open: ProviderOpen, window: &mut Window, cx: &mut Context<Self>) {
        let ProviderOpen { provider_id, mut provider, events, session_id, project, workspace, title, epoch, lease_held, stored_turns, fresh_thread_model } =
            open;
        // Last ask wins: an earlier open finishing after a newer one was
        // asked for never steals focus or the send — its child is shut
        // down and nothing else is touched. The pending switch belongs to
        // the current open, so it stays set; no record, no row, no view.
        if !self.open_state.is_current_open(epoch) {
            crate::baaz_log!(
                "provider lane superseded provider={} session={session_id}",
                provider_id.as_str()
            );
            provider.shutdown();
            self.browser.update(cx, |browser, _| browser.drop_untargeted_parcels());
            return;
        }
        // A waiting parcel's session just earned its id: key the parcel to
        // it before the view lands, so `activate` hands it over here — and
        // only here.
        self.browser.update(cx, |browser, _| browser.key_pending_parcels(&session_id));
        self.load_menu_sources(std::path::PathBuf::from(workspace.clone()), cx);
        // The terminal relay's route lands here too: the ack's session id
        // is what the lane serves from now on — for Codex that is the
        // server-minted thread id, which no pre-registration could name.
        self.register_terminal_session(
            &session_id,
            std::path::PathBuf::from(workspace.clone()),
            provider_id.as_str(),
            cx,
        );
        let overlays = self.overlays.clone();
        let host = SessionHost {
            provider_id: provider_id.as_str().to_owned(),
            workspace: workspace.clone(),
            overlays,
            capture: self.capture.clone(),
            terminal_host: Some(self.terminal_host(cx)),
        };
        let view = cx.new(|cx| {
            SessionView::new_on_provider(session_id.clone(), provider, events, host, window, cx)
        });
        if let Some(started_on) = fresh_thread_model {
            view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, _| rt.note_fresh_thread(started_on)));
        }
        // A disabled open lands readable with the quiet banner instead of
        // a live child: the view above rides the scripted lane, so there
        // is no child to hang up and nothing was ever sent.
        if self.open_state.pending_disabled_notice == Some(provider_id) {
            self.open_state.pending_disabled_notice = None;
            view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.show_disabled_notice(provider_id, cx)));
        }
        // Like the muse path: the session groups under the project it
        // started in, and the drafts map names it while it is unsent so a
        // repeated ⌘N reopens it instead of spawning another child. Only a
        // fresh open is a draft: a reopen or a fork lands a session that
        // already has a record, and naming it here made the next ⌘N
        // "reuse" whatever was clicked last — a handoff destination's row
        // reads zero turns, so it passed for unsent and ⌘N did nothing.
        // Every provider's session has a record since P4-B (docs/70): a
        // Muse reopen writes one from its list row before it gets here.
        let fresh = !self.session_list.read(cx).provider_sessions.contains_key(&session_id);
        if fresh {
            self.open_state.name_draft(project.clone(), &session_id);
        }
        // Where the session runs: what a scripted check reads to prove a
        // project-less session never starts in `/` (docs/82 0.9).
        crate::baaz_log!("provider lane workspace session={session_id} workspace={workspace}");
        self.record_provider_open(provider_id, &session_id, project, workspace, title, cx);
        // Baaz's own store held the history (docs/85 11.1): it is shown at
        // once, and nothing pages. Otherwise a provider that pages its
        // history (Muse) attached without it: an existing session's turns
        // page in now.
        let from_store = stored_turns.is_some();
        if let Some(turns) = stored_turns {
            crate::baaz_log!("reopen from the transcript store: session={session_id} turns={}", turns.len());
            view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.load_stored_turns(turns, cx)));
        }
        if !fresh && !from_store && provider_id.has(provider::ProviderTrait::PagesHistory) {
            view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.backfill_lane(cx)));
        }
        // Another window holds the session: it still opens, read-only under
        // the lease notice, with no dialog — the row stays selectable.
        if lease_held {
            view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.set_lease_lost(crate::app::LEASE_NOTICE, cx)));
        }
        // The swap that makes it the visible session: parks the outgoing
        // view, points the event subscription at the new one, focuses the
        // composer — everything `open` does for muse.
        self.activate(view, false, window, cx);
        // A handoff's destination lands here: the pack submits onto the
        // fresh view. Anything else leaves the pending handoff alone.
        self.land_handoff_destination(session_id.clone(), cx);
        // A reopened handoff destination renders its snapshot prefix (or
        // the fallback divider) above the replayed history. Fresh opens
        // have no links yet and pass through untouched.
        if let Some(view) = self.active.clone().filter(|view| view.read(cx).rt(cx).session_id == session_id) {
            self.attach_handoff_prefix(&view, cx);
        }
        // The scriptable check: a `--steps` run greps the log for this line
        // to prove the pick ended on a provider lane, not a muse session.
        // (`baaz_log!` already carries the `baaz: ` prefix; spelling it
        // here doubled it to `baaz: baaz:`.)
        crate::log::trace_mark("provider lane open");
        crate::log::trace_arm_first_frame();
        crate::baaz_log!(
            "provider lane open provider={} session={session_id}",
            provider_id.as_str()
        );
    }

    /// What a landed provider session leaves behind, wherever its view
    /// opened: the project it files under, and the local record with its
    /// (still empty) sidebar row.
    fn record_provider_open(
        &mut self,
        provider_id: ProviderId,
        session_id: &str,
        project: Option<String>,
        workspace: String,
        title: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = project.clone() {
            self.set_override(session_id, |meta| meta.project = Some(id.clone()), cx);
        }
        // The local record, for every provider (docs/70): it routes the
        // next reopen through `reopen_provider` and holds Baaz's own facts
        // (turns, first prompt, handoff links). For Claude Code and Codex
        // it is also what brings the lane back after a restart — the
        // sidebar reads it, and the click reopens it. Written before the
        // row below is built, so the two never disagree.
        self.session_list.update(cx, |list, _| {
            crate::provider_sessions::upsert_open(
                &mut list.provider_sessions,
                &mut list.overrides,
                provider_id.as_str(),
                session_id,
                Some(workspace.clone()),
                project.clone(),
                title,
            )
        });
        self.persist_provider_sessions(cx);
        // A session in one of Baaz's worktrees: its row ties to the
        // worktree's, queued behind the record just written.
        self.link_worktree_session(provider_id, session_id, &workspace, cx);
        // A provider that lists its own sessions (Muse) owns their rows:
        // the record never builds one. Its row goes in now from what the
        // open knows — zero turns, so out of view until the first send
        // titles it — and no `session/list` round trip is spent on one row.
        if provider_id.has(provider::ProviderTrait::NativeSessionList) {
            self.ensure_listed_row(session_id, project, workspace, cx);
            return;
        }
        // No row with content yet: like a muse draft, the session earns
        // its row on the first send (`ProviderTurnAccepted`), titled from
        // the prompt — but the record above already exists, so a restart
        // before that still reopens it.
        self.merge_provider_rows(cx);
    }

    /// A local row for a session of a provider that lists its own
    /// sessions, when the list has not shown it yet: titled "New session",
    /// zero turns (so out of view until its first send), and not running —
    /// opened, not started; the first turn's start arms it. The list's own
    /// row replaces it when it arrives ([`sidebar::merge_session_list`]).
    pub(super) fn ensure_listed_row(&mut self, session_id: &str, project: Option<String>, workspace: String, cx: &mut gpui::App) {
        if self.session_list.read(cx).sessions.iter().any(|entry| entry.id == session_id) {
            return;
        }
        let mut row = sidebar::local_started_row(
            session_id,
            sidebar::UNNAMED.to_owned(),
            project,
            Some(workspace),
            crate::clock::now_local(),
        );
        row.running = false;
        self.session_list.update(cx, |list, _| list.sessions.push(row));
        self.invalidate_list(cx);
    }

    /// "New session" on a provider pick (Claude Code / Codex). A plain new
    /// session draws its view on the click, like a muse draft: the view
    /// opens under the open's request id with no provider attached, and
    /// the child is attached when `OpenSession` answers
    /// ([`Self::land_provider_draft`]) — measured, the Codex handshake
    /// alone took 0.3–1.7 s, and a `/` typed meanwhile was lost. Anything
    /// else keeps the round trip first ([`Self::open_on_provider`]): a
    /// switch holds its old view up until the replacement lands (Y2b), a
    /// pending handoff submits its pack on the landing, and the scripted
    /// and disabled lanes open synchronously anyway.
    pub(super) fn open_new_on_provider(
        &mut self,
        provider_id: ProviderId,
        project: Option<String>,
        workspace: String,
        switch: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A provider switch of an unsent draft draws its replacement on
        // the click too: `activate` closes the replaced draft in the same
        // update (Y2b) — its typed text moves across, its child is hung up
        // — so nothing of the old provider (placeholder, model chip,
        // banner) stays up while the new child answers.
        let switching = switch.is_some_and(|epoch| self.open_state.replaces_under(epoch));
        let early = (switching || (switch.is_none() && !self.open_state.is_replacing()))
            && self.handoff_state.pending.is_none()
            && !self.providers.lands_inline()
            && crate::provider_status::provider_enabled(provider_id);
        if !early {
            self.open_on_provider(provider_id, project, workspace, window, cx);
            return;
        }
        crate::log::boot_mark("new-draft-open");
        self.load_menu_sources(std::path::PathBuf::from(workspace.clone()), cx);
        let mode = self.provider_open_mode(provider_id, Some(&workspace), project.as_deref(), cx);
        // The request id is the view's id until the child answers: Claude
        // Code takes it as `--session-id`, so it stays; Codex mints a
        // thread id, and the landing renames everything keyed by this one
        // (see `rekey_session`). Registered with the terminal relay before
        // the open runs, like `open_on_provider` does.
        let request_id = provider::new_request_id();
        self.register_terminal_session(
            &request_id,
            std::path::PathBuf::from(workspace.clone()),
            provider_id.as_str(),
            cx,
        );
        self.browser.update(cx, |browser, _| browser.key_pending_parcels(&request_id));
        let host = SessionHost {
            provider_id: provider_id.as_str().to_owned(),
            workspace: workspace.clone(),
            overlays: self.overlays.clone(),
            capture: self.capture.clone(),
            terminal_host: Some(self.terminal_host(cx)),
        };
        let view = cx.new(|cx| SessionView::new_provider_opening(request_id.clone(), mode, host, window, cx));
        // A switch carries the replaced draft's mode and effort; otherwise
        // the last effort picked on this provider is the level the first
        // turn asks for (owner 2026-10-21: a Claude Code draft never read
        // it). Then the last model picked for this provider is the model
        // the draft asks for — the chip names it and the open's landing
        // sends it (on Codex the open itself names it, at the thread's
        // start), so chip, menu check and child agree from the first
        // frame.
        let picks = self.session_list.read(cx).projects.picks(project.as_deref().and_then(|id| self.session_list.read(cx).projects.find(id)));
        // A restart on a model (`RestartOnModel`) carries the pick, which
        // stands over the remembered one.
        let carry = self.open_state.take_carry();
        let mut carried_model = None;
        if let Some((mode, effort, model)) = switch.and(carry) {
            view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.carry_draft_settings(mode, effort, cx)));
            carried_model = model;
        } else if let Some(effort) = picks.effort_for(provider_id.as_str()).as_deref().and_then(projects::parse_effort) {
            view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.set_initial_effort(Some(effort), cx)));
        }
        let model = carried_model.or_else(|| self.start_model(provider_id, project.as_deref(), cx));
        // On a provider whose thread takes its model at its start, the open
        // names it too ([`thread_start_model`]); noted first, so the pick
        // below queues like any other rather than restarting this draft.
        let start_model = thread_start_model(provider_id, model.clone());
        view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, _| rt.note_fresh_thread(start_model.clone())));
        if let Some(model) = model {
            view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.set_model(&model, cx)));
        }
        // The open asks for the mode the draft now stands on: a carried or
        // project Auto on a remembered model without it already fell back
        // to Ask above, and the child starts there rather than in Auto.
        let mode = view.read(cx).rt(cx).draft_settings().0;
        // Named before the swap, so `activate` reads the project's name
        // for the empty state off the drafts map (no row exists yet), and
        // a repeated ⌘N while the child is still answering reopens this
        // view instead of spawning a second child.
        self.open_state.name_draft(project.clone(), &request_id);
        self.activate(view, false, window, cx);
        // Scripted session verbs wait for the open (see `run_steps`), as
        // they wait for a muse draft's start.
        self.open_state.starts_in_flight.insert(request_id.clone());
        crate::log::boot_mark("new-draft-shown");
        crate::log::trace_mark("new-draft-shown");
        crate::log::trace_arm_first_frame();
        self.spawn_provider_open(provider_id, project, workspace, request_id, mode, start_model, cx);
    }

    /// The background half of a provider draft's open: run the provider's
    /// open off the UI thread, land it on the view keyed `request_id`
    /// wherever that view is ([`Self::land_provider_draft`]) or fail it, and
    /// fail it visibly past [`PROVIDER_OPEN_TIMEOUT`]. Shared by the centre's
    /// drafts and browser chats.
    // `model` joined the open's facts with the model-at-thread-start fix:
    // each is one of the open's own inputs, with no grouping that both
    // callers already hold.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn spawn_provider_open(
        &mut self,
        provider_id: ProviderId,
        project: Option<String>,
        workspace: String,
        request_id: String,
        mode: aui_protocol::PermissionMode,
        model: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let factory = self.providers.clone();
        let workspace_bg = workspace.clone();
        let opened = request_id.clone();
        let epoch = self.open_state.open_epoch();
        // Where Retry starts again when this open fails (the view is gone
        // by then): cloned before `work` moves the originals.
        let (retry_project, retry_workspace) = (project.clone(), workspace.clone());
        let (timed_project, timed_workspace) = (project.clone(), workspace.clone());
        let mut extras = self.open_extras(provider_id, mode, Some(&workspace), project.as_deref(), cx);
        extras.model = thread_start_model(provider_id, model);
        let cancelled = std::sync::Arc::clone(&self.open_state.provider_opens_cancelled);
        let work = move || {
            provider_open_work(factory, provider_id, opened, workspace_bg, extras, project, workspace, epoch, Some(cancelled))
        };
        let timed = request_id.clone();
        self.wire_call_in(cx, work, move |this, result, window, cx| match result {
            Ok(open) => {
                this.terminal_service.alias_session(&request_id, &open.session_id, cx);
                this.land_provider_draft(&request_id, open, window, cx);
            }
            Err(error) => {
                crate::baaz_log!("provider open failed ({}): {error}", provider_id.label());
                this.fail_provider_draft(&request_id, provider_id, error, retry_project, retry_workspace, cx);
            }
        });
        // A child that never answers must not leave the view opening
        // forever: past the timeout it fails, visibly, and a late answer
        // finds no opening view and is hung up (`land_provider_draft`).
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PROVIDER_OPEN_TIMEOUT).await;
            let _ = this.update(cx, |this, cx| {
                this.expire_provider_open(&timed, provider_id, timed_project, timed_workspace, cx)
            });
        });
        self.wire_tasks().push(task);
    }

    /// A provider draft's open outlived [`PROVIDER_OPEN_TIMEOUT`]: fail
    /// its view the way a refused open does, when it is still opening.
    pub(crate) fn expire_provider_open(
        &mut self,
        provisional: &str,
        provider_id: ProviderId,
        project: Option<String>,
        workspace: String,
        cx: &mut Context<Self>,
    ) {
        let opening = self.find_view(provisional, cx).is_some_and(|view| view.read(cx).rt(cx).is_starting());
        if !opening || !self.open_state.starts_in_flight.contains(provisional) {
            return;
        }
        crate::baaz_log!("provider open timed out ({}) session={provisional}", provider_id.label());
        let reason = format!("{} did not answer within {}s", provider_id.label(), PROVIDER_OPEN_TIMEOUT.as_secs());
        self.fail_provider_draft(
            provisional,
            provider_id,
            crate::app::OpenFailed::from(provider::ProviderError::Unavailable { reason }),
            project,
            workspace,
            cx,
        );
    }

    /// Forget everything the window keyed by a provider draft's provisional
    /// id once its view is gone without landing (closed, replaced,
    /// evicted, or its open answered too late): the draft and unsent
    /// names — so ⌘N never reopens a dead view — the scripted-verb wait,
    /// the terminal provider, and any per-session override a toggle
    /// persisted meanwhile.
    pub(super) fn forget_provisional(&mut self, provisional: &str, cx: &mut Context<Self>) {
        self.open_state.forget_opening(provisional);
        self.terminal_dock.update(cx, |dock, _| dock.forget_provider(provisional));
        if self.session_list.update(cx, |list, _| list.overrides.remove(provisional).is_some()) {
            crate::sessions::write(&self.session_list.read(cx).overrides);
            self.invalidate_list(cx);
        }
    }

    /// The child behind a provider draft answered: attach it to the view
    /// drawn on the click — wherever that view is now, active or parked —
    /// under the session's real id, record the session, then replay what
    /// the person did meanwhile (`start_landed`). The view is never
    /// re-activated: the person may have moved on. A draft that is gone (a
    /// provider switch replaced it) leaves its child to nobody, so the
    /// child is hung up.
    fn land_provider_draft(
        &mut self,
        provisional: &str,
        open: ProviderOpen,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::log::boot_mark("provider open-reply early");
        let ProviderOpen { provider_id, mut provider, events, session_id, project, workspace, title, fresh_thread_model, .. } = open;
        self.open_state.starts_in_flight.remove(provisional);
        // Baaz is quitting: every lane child was hung up, and one that
        // answers after the teardown is hung up too, never attached.
        if self.open_state.provider_opens_cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            provider.shutdown();
            return;
        }
        let Some(view) = self.find_view(provisional, cx).filter(|view| view.read(cx).rt(cx).is_starting()) else {
            crate::baaz_log!(
                "provider draft gone before its open landed provider={} session={session_id}",
                provider_id.as_str()
            );
            provider.shutdown();
            self.forget_provisional(provisional, cx);
            return;
        };
        if session_id != provisional {
            self.rekey_session(provisional, &session_id, cx);
        }
        self.register_terminal_session(
            &session_id,
            std::path::PathBuf::from(workspace.clone()),
            provider_id.as_str(),
            cx,
        );
        view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.attach_provider(session_id.clone(), provider, events, cx)));
        // The model the thread started on: the draft noted the one it
        // asked for, and this is what the open carried.
        if let Some(started_on) = fresh_thread_model {
            view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, _| rt.note_fresh_thread(started_on)));
        }
        // Recorded before the queue replays, so a send's acceptance finds
        // the record it titles.
        // Where the session runs: what a scripted check reads to prove a
        // project-less session never starts in `/` (docs/82 0.9).
        crate::baaz_log!("provider lane workspace session={session_id} workspace={workspace}");
        self.record_provider_open(provider_id, &session_id, project, workspace, title, cx);
        view.update(cx, |view, cx| view.start_landed(window, cx));
        crate::log::trace_mark("provider lane open");
        crate::baaz_log!(
            "provider lane open provider={} session={session_id}",
            provider_id.as_str()
        );
    }

    /// A provider draft's open failed (its child died on its first breath,
    /// refused, or never answered). There is no session behind the draft,
    /// so the window lands on the typed failure view a failed open gets
    /// ([`LaunchFailure`]: the kind's sentence, the child's stderr, Retry,
    /// the kind's action, Providers…) instead of the draft under a red
    /// "did not start" banner with its send button stuck (QA 2026-10-30).
    /// The draft's words — typed, or queued on the open — ride Retry back
    /// into the new draft. A draft that is no longer the centre's (the
    /// person moved on) keeps the old quiet contract: its view says why,
    /// the sends go back in its composer. Either way the next "New
    /// session" starts fresh.
    fn fail_provider_draft(
        &mut self,
        provisional: &str,
        provider_id: ProviderId,
        failed: crate::app::OpenFailed,
        project: Option<String>,
        workspace: String,
        cx: &mut Context<Self>,
    ) {
        self.open_state.forget_opening(provisional);
        self.browser.update(cx, |browser, _| browser.drop_untargeted_parcels());
        let reason = failed.error.to_string();
        let Some(view) = self.find_view(provisional, cx) else {
            cx.notify();
            return;
        };
        // The queued sends go back in the composer first, so the words read
        // from one place below.
        view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.start_failed(reason.clone(), cx)));
        let centred = self.active.as_ref().is_some_and(|active| active.read(cx).rt(cx).session_id == provisional);
        if !centred {
            cx.notify();
            return;
        }
        let draft = view.read(cx).words_in_hand(cx);
        self.close_view(provisional, cx);
        self.resubscribe_live_views(cx);
        self.open_state.switch_pending = false;
        let kind = crate::provider_status::FailureKind::classify(provider_id, Some(&failed.error));
        self.open_state.provider_open_error = Some(LaunchFailure {
            session_id: None,
            provider: provider_id,
            kind,
            message: reason,
            stderr_excerpt: failed.stderr,
            retry: ProviderOpenRetry::OpenNew { project, workspace, draft },
        });
        cx.notify();
    }

    /// Move everything the window keyed by a provider draft's provisional
    /// id onto the id its child minted (Codex's thread id): the parked
    /// view's slot, the drafts and unsent names, the selection and reveal,
    /// the parcels waiting on it, a per-session override (the pane state a
    /// toggle wrote meanwhile), any session browser tabs, and a browser
    /// chat's used pages and reveal (docs/81 A F4). The view
    /// renames itself in `attach_provider`. The draft has no row, no
    /// record and no turn until it lands.
    pub(super) fn rekey_session(&mut self, old: &str, new: &str, cx: &mut Context<Self>) {
        self.registry.rekey(old, new);
        // The drafts and unsent names, the selection, and a switch away
        // from this draft made while its child was still answering (its
        // marker and claim), all under the real id.
        self.open_state.rekey(old, new);
        // The landing registers the real id; the provisional one goes.
        self.terminal_dock.update(cx, |dock, _| dock.forget_provider(old));
        // An armed reveal (and its wait for a row) follows the real id.
        self.sidebar_pane.update(cx, |pane, _| pane.rekey_reveal(old, new));
        self.browser.update(cx, |browser, _| browser.rekey_parcels(old, new));
        if let Some(meta) = self.session_list.update(cx, |list, _| list.overrides.remove(old)) {
            self.session_list.update(cx, |list, _| list.overrides.insert(new.to_owned(), meta));
            crate::sessions::write(&self.session_list.read(cx).overrides);
            self.invalidate_list(cx);
        }
        if let Some(pages) = self.browser.update(cx, |browser, _| browser.agent_window.browser_chat_pages.remove(old)) {
            self.browser.update(cx, |browser, _| browser.agent_window.browser_chat_pages.insert(new.to_owned(), pages));
        }
        let owner = crate::browser::Owner::Session(old.to_owned());
        let tabs: Vec<_> = self.browser.read(cx).tabs.tabs_of(&owner).iter().map(|tab| tab.id).collect();
        for tab in tabs {
            self.browser.update(cx, |bv, _| bv.tabs.move_to(tab, crate::browser::Owner::Session(new.to_owned()), None));
        }
        cx.notify();
    }

    /// A provider open failed (Z4). With no switch to restore, the window
    /// lands on the failed open itself: the outgoing view parks exactly
    /// like a normal switch away (not closed, not lost), the selection
    /// names the failed session, and the centre shows the inline failure
    /// instead of a modal dialog over another session's transcript. Retry
    /// re-runs the open through the normal activation path.
    ///
    /// A provider SWITCH that fails keeps its old contract: the old view
    /// is restored with its chip and the reason dialogs (Y2b).
    fn fail_provider_open(
        &mut self,
        provider: ProviderId,
        failed: crate::app::OpenFailed,
        retry: ProviderOpenRetry,
        session_id: Option<String>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let kind = crate::provider_status::FailureKind::classify(provider, Some(&failed.error));
        let reason = failed.error.to_string();
        let stderr_excerpt = failed.stderr;
        if self.open_state.is_replacing() {
            self.abandon_switch(cx);
            self.fail_pending_handoff(reason.clone(), cx);
            self.set_dialog(
                cx,
                Dialog {
                    title: format!("Couldn't start {}", provider.label()),
                    detail: reason,
                    kind: DialogKind::Error,
                    primary: "Dismiss",
                    action: DialogAction::Dismiss,
                    archive_target: None,
                },
            );
            return;
        }
        self.fail_pending_handoff(reason.clone(), cx);
        self.park_active(cx);
        self.resubscribe_live_views(cx);
        self.open_state.switch_pending = false;
        // A failed open lands no session: a parcel waiting on it has
        // nowhere to go.
        self.browser.update(cx, |browser, _| browser.drop_untargeted_parcels());
        if let Some(id) = session_id.clone() {
            self.open_state.pending_id = Some(id.clone());
            self.restore_right_for_session(&id, cx);
        }
        self.open_state.provider_open_error =
            Some(LaunchFailure { session_id, provider, kind, message: reason, stderr_excerpt, retry });
        cx.notify();
    }

    /// Retry on the inline failure state re-runs the failed open. A
    /// success lands through the normal activation path (including the
    /// per-session right-pane restore `activate` performs); a failure
    /// replaces the standing error.
    pub(crate) fn retry_provider_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(failure) = self.open_state.provider_open_error.clone() else { return };
        match failure.retry {
            ProviderOpenRetry::Reopen(record) => self.reopen_provider(*record, window, cx),
            // A failed draft's words come back in the new draft, which
            // opens on the click like any New session (the draft path).
            ProviderOpenRetry::OpenNew { project, workspace, draft } if !draft.is_empty() => {
                self.open_state.provider_open_error = None;
                self.open_new_on_provider(failure.provider, project, workspace, None, window, cx);
                if let Some(view) = self.active.clone().filter(|view| view.read(cx).rt(cx).is_starting()) {
                    view.update(cx, |view, cx| view.set_draft(draft, window, cx));
                }
            }
            ProviderOpenRetry::OpenNew { project, workspace, .. } => {
                self.open_on_provider(failure.provider, project, workspace, window, cx)
            }
        }
    }

    /// Reopen a stored provider session after a restart: connect a fresh
    /// child through the factory and `ResumeSession` it there, then land
    /// it like any lane open. The adapter replays the transcript's deltas
    /// (Claude Code its `~/.claude` jsonl, Codex its thread), which the
    /// lane folds into the view — except a record with no settled turns,
    /// which holds no history anywhere: it opens fresh and the stale
    /// record leaves with it. A refusal lands inline on the clicked
    /// session (see [`Self::fail_provider_open`]) — never a silent muse
    /// fallback, never a fresh empty session under the old id.
    pub(crate) fn reopen_provider(
        &mut self,
        record: crate::provider_sessions::ProviderSessionRecord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A reopen is an ask like any other: it joins the last-wins epoch
        // so a newer open is never stolen by its late finish.
        let epoch = self.open_state.begin_open();
        // Q4b: the old view's composer keeps no keystrokes while hidden —
        // consume the focus arm at the click (the landing view arms its own
        // through `activate`, and the settle drops any arm raised
        // mid-switch), so typing lands nowhere until the destination opens.
        self.window_sync.focus_composer = false;
        // A record naming no provider this build knows is refused — never
        // reopened as some other provider.
        let Some(provider_id) = ProviderId::parse_known(&record.provider) else {
            crate::baaz_log!("provider reopen refused: unknown provider {:?}", record.provider);
            self.abandon_switch(cx);
            self.set_dialog(
                cx,
                Dialog {
                    title: "Couldn't reopen session".into(),
                    detail: format!("Unknown provider {:?}", record.provider),
                    kind: DialogKind::Error,
                    primary: "Dismiss",
                    action: DialogAction::Dismiss,
                    archive_target: None,
                },
            );
            return;
        };
        // An untouched draft — a record with no settled turns and no
        // prompt ever sent — holds no history anywhere: resuming it would
        // replay nothing, so it opens fresh instead, and the stale record
        // leaves with it so the sidebar never accumulates dead rows. (A
        // record whose only turn was cut off carries `firstPrompt` with
        // `turns == 0`: that prompt must resume, not be dropped.) A
        // session its provider lists counts as untouched only when its
        // list row says so: a record written with no row to read (boot
        // `--session` before the list) never drops a real session.
        // The record's project and title are the session row's own facts.
        let (stored_project, stored_title) = self
            .session_list.read(cx).overrides
            .get(&record.session_id)
            .map(|meta| (meta.project.clone(), meta.provider_title.clone()))
            .unwrap_or_default();
        if self.record_holds_no_history(&record, cx) {
            crate::baaz_log!(
                "provider reopen: {} has no turns, opening fresh",
                record.session_id
            );
            self.session_list.update(cx, |list, _| crate::provider_sessions::remove(&mut list.provider_sessions, &record.session_id));
            self.persist_provider_sessions(cx);
            self.merge_provider_rows(cx);
            let workspace = record.workspace.clone().unwrap_or_else(|| self.unfiled_workspace());
            self.open_on_provider(provider_id, stored_project, workspace, window, cx);
            return;
        }
        // Q4c: the right pane stays neutral at the click — it describes
        // neither session until the open lands. Restoring the
        // destination's pane here would apply its files state onto the
        // previous session's project root (`right_project` still reads
        // the previous session) while the previous session's webview
        // draws under the loading centre. The previous view stays in
        // `active` (unparked, draft intact) until `finish_provider_open`
        // parks it and restores the destination's pane there, or
        // `fail_provider_open` parks it onto the inline failure, or the
        // switch is cancelled and the previous pane is restored as it
        // was; only the rendered centre moves now.
        // A disabled provider reopens with no child either: the scripted
        // stand-in's resume below replays the stored transcript read-only,
        // and the landing view wears the quiet banner. On the window's
        // scripted stand-in (`--no-connect` / `--replay`) the stand-in
        // answers `OpenSession` but no resume — the reopen still goes
        // through `ResumeSession`, the same work as a live one, so the
        // failure is the honest one.
        let factory = self.lane_factory(provider_id);
        let inline = factory.lands_inline();
        // A provider whose sessions share one child: the window starts it
        // now, and the factory below waits while it comes up.
        self.ensure_provider_started(provider_id, cx);
        let workspace = record.workspace.clone().unwrap_or_else(|| self.workspace(cx));
        // The terminal relay's route: the resumed session's bridge answers
        // for the stored id, so it is (re-)registered BEFORE the resume
        // runs — refreshing the root it drives — unless the shared child
        // refused the resume's bridge once this run (the hub's memo).
        if !self.resume_bridge_refused(provider_id) {
            self.register_terminal_session(
                &record.session_id,
                std::path::PathBuf::from(workspace.clone()),
                record.provider.as_str(),
                cx,
            );
        }
        // What Retry re-runs when this reopen fails: cloned up front,
        // because `work` below borrows the record into the background.
        let retry_reopen = ProviderOpenRetry::Reopen(Box::new(record.clone()));
        let retry_id = record.session_id.clone();
        // The gate reads the registry off the UI thread: cloned up front
        // for the same reason.
        let registry = self.session_list.read(cx).provider_sessions.clone();
        let muse_stamp = self.session_list.read(cx).index.get(&record.session_id).and_then(|entry| entry.updated_at_us);
        let mode = self.provider_open_mode(provider_id, Some(&workspace), stored_project.as_deref(), cx);
        let work = move || -> (Result<ProviderOpen, crate::app::OpenFailed>, usize) {
            // The consent gate, on the background executor: before the
            // person's Move click an old session resumes from its
            // original location — the one child pinned on the owner's
            // home — and nothing moves; after it the lazy move runs
            // here, ordered before the resume below, through the same
            // executor the prompt uses. Sessions with nothing
            // owner-side move nothing either way.
            let owner = crate::session_migration::owner_home();
            let state = crate::store::support_dir();
            let moved = match crate::session_migration::gate_resume(
                &owner,
                &state,
                &registry,
                &record.provider,
                &record.session_id,
            ) {
                crate::session_migration::ResumeGate::LegacyOwner => {
                    crate::session_migration::pin_legacy_resume_for(&record);
                    0
                }
                crate::session_migration::ResumeGate::LazyMove => {
                    let moved = crate::session_migration::ensure_session_moved(
                        &owner,
                        &state,
                        &record.provider,
                        &record.session_id,
                    );
                    if moved > 0 {
                        crate::session_migration::clear_legacy_resume(
                            &record.provider,
                            &record.session_id,
                        );
                    }
                    moved
                }
                crate::session_migration::ResumeGate::BaazHome => 0,
            };
            if moved > 0 {
                crate::baaz_log!("provider reopen: lazy-moved {moved} file(s) for {}", record.session_id);
            }
            crate::log::trace_mark("reopen gate done");
            let result = (|| -> Result<ProviderOpen, crate::app::OpenFailed> {
                let provider = factory.open(provider_id)?;
                crate::log::trace_mark("reopen adapter connected");
                #[cfg(not(all(test, feature = "probe")))]
                let (provider, events) = conn::gate(provider);
                // Two refusals still land the lane (docs/70 §4), for any
                // adapter that raises them: a stale history the resume
                // already regenerated pages in anyway, and a session
                // another window holds opens read-only under the lease
                // notice. Any other refusal fails the reopen.
                // A worktree session's child gets its variables back after
                // a restart, as its open gave them (docs/82 6.2).
                let env = record
                    .workspace
                    .as_deref()
                    .map(|folder| baaz_core::worktrees::env_for_folder(&state, std::path::Path::new(folder)))
                    .unwrap_or_default();
                // Baaz's own store first (docs/85 11.1): a fresh copy is
                // shown as it is and the provider resumes without history.
                let stored_turns = crate::transcripts::kept_if_fresh(&state, &record.provider, &record.session_id, muse_stamp);
                crate::log::trace_mark(if stored_turns.is_some() { "reopen from store" } else { "reopen from provider" });
                let (session_id, lease_held) =
                    match crate::provider_sessions::send_resume(&provider, &record, &provider::new_request_id(), Some(mode), env, stored_turns.is_some()) {
                        Ok(session_id) => (session_id, false),
                        Err(error) if error.hint() == provider::RefusalHint::StaleHistory => {
                            crate::baaz_log!("resume of {} hit a stale history ({error}); paging anyway", record.session_id);
                            (record.session_id.clone(), false)
                        }
                        Err(error) if error.hint() == provider::RefusalHint::HeldElsewhere => {
                            crate::baaz_log!("resume of {} rejected ({error}); lease notice on the view", record.session_id);
                            (record.session_id.clone(), true)
                        }
                        // The child goes with the provider: its stderr
                        // travels with the refusal (docs/71 §3).
                        Err(error) => return Err(crate::app::OpenFailed { error, stderr: provider.stderr_tail() }),
                    };
                crate::log::trace_mark("reopen resume acked");
                // Tests drain after the resume, synchronously on the test
                // executor: the replayed deltas are buffered before the lane
                // starts, with no forwarding thread.
                #[cfg(feature = "probe")]
                #[cfg(test)]
                let (provider, events) = conn::gate_sync(provider);
                Ok(ProviderOpen {
                    provider_id,
                    provider,
                    events,
                    session_id,
                    project: stored_project.clone(),
                    workspace: record.workspace.clone().unwrap_or(workspace),
                    title: stored_title.clone(),
                    epoch,
                    lease_held,
                    stored_turns,
                    fresh_thread_model: None,
                })
            })();
            (result, moved)
        };
        self.run_open(inline, work, move |this, outcome, window, cx| {
            let (result, moved) = outcome;
            // The move changed what waits: refresh the cached plan off the
            // UI thread so the Providers row follows the lazy move.
            if moved > 0 {
                this.invalidate_migration_cache(cx);
            }
            match result {
            Ok(open) => {
                // The bridge was spawned under the stored id; if the resume
                // answered another, the agent's tools must still reach this
                // session's browser and terminal.
                if open.session_id != retry_id {
                    this.terminal_service.alias_session(&retry_id, &open.session_id, cx);
                }
                this.finish_provider_open(open, window, cx);
            }
            Err(error) => {
                if !this.open_state.is_current_open(epoch) {
                    crate::baaz_log!(
                        "provider resume failed for a stale ask ({}): {error}",
                        provider_id.label()
                    );
                    return;
                }
                crate::baaz_log!("provider resume failed ({}): {error}", provider_id.label());
                this.fail_provider_open(
                    provider_id,
                    error,
                    retry_reopen,
                    Some(retry_id),
                    window,
                    cx,
                );
            }
            }
        }, window, cx);
    }

    /// Forget a provider session for good: the record, its row and its
    /// views leave together. Dropping a view hangs its child up (see
    /// `SessionRuntime::drop`), so deleting the open or parked session also
    /// shuts the child down — no orphaned `claude` / `codex` process.
    /// muse sessions never reach here: their list is the server's, and
    /// Baaz only ever hides or archives them.
    ///
    /// What Baaz kept for it goes too (D-8), off the UI thread: its row in
    /// `baaz.db` with its turns and attachments, its checkpoint refs in its
    /// folder's repository (docs/82 6.3), its sent pictures and its
    /// hand-off snapshot. A transcript the provider wrote in Baaz's own home
    /// is never removed silently: the person is asked whether to move it to
    /// the Trash ([`Self::offer_transcript_trash`]).
    pub(crate) fn delete_provider_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        let forgotten = self
            .session_list.read(cx).provider_sessions
            .get(session_id)
            .map(|record| (record.provider.clone(), record.is_imported(), record.workspace.clone()));
        if self.active.as_ref().is_some_and(|view| view.read(cx).rt(cx).session_id == session_id) {
            self.active = None;
        }
        // Nor does the header or the selection keep naming it.
        if self.open_state.pending_id.as_deref() == Some(session_id) {
            self.open_state.pending_id = None;
        }
        // A deleted session takes its inline open failure with it (Z4), so
        // Retry cannot resurrect what was just forgotten.
        if self
            .open_state.provider_open_error
            .as_ref()
            .is_some_and(|failure| failure.session_id.as_deref() == Some(session_id))
        {
            self.open_state.provider_open_error = None;
        }
        self.registry.forget_where(|id| id == session_id);
        self.forget_terminal_tasks(&[session_id.to_owned()], cx);
        self.session_list.update(cx, |list, _| list.sessions.retain(|entry| entry.id != session_id));
        self.session_list.update(cx, |list, _| list.overrides.remove(session_id));
        self.session_list.update(cx, |list, _| crate::provider_sessions::remove(&mut list.provider_sessions, session_id));
        self.persist_provider_sessions(cx);
        if let Some((provider, imported, folder)) = forgotten {
            let id = session_id.to_owned();
            let folder = folder.map(std::path::PathBuf::from);
            self.wire_call(cx, move || crate::sessions::forget(&provider, &id, imported, folder.as_deref()), |this, forgotten, cx| {
                this.offer_transcript_trash(forgotten.transcript, cx);
            });
        }
        self.rejoin(cx);
        self.invalidate_list(cx);
    }

    /// Ask whether a forgotten session's transcript, still in Baaz's own
    /// home for its provider, should go to the Trash. Nothing moves without
    /// the person's yes ([`Self::trash_transcript`]); closing the dialog any
    /// other way keeps the files.
    pub(crate) fn offer_transcript_trash(&mut self, transcript: Vec<std::path::PathBuf>, cx: &mut Context<Self>) {
        if transcript.is_empty() {
            return;
        }
        let paths: Vec<String> = transcript.iter().map(|path| path.to_string_lossy().into_owned()).collect();
        self.set_dialog(
            cx,
            crate::overlays::Dialog {
                title: "Move its transcript to the Trash?".into(),
                detail: "The session is gone from Baaz. Its transcript is still in Baaz's folder. \
                    You can restore it from the Trash."
                    .into(),
                kind: aui::overlay::DialogKind::Warning,
                primary: "Move to Trash",
                action: crate::overlays::DialogAction::TrashTranscript,
                archive_target: Some(paths.join("\n")),
            },
        );
    }

    /// The Trash dialog's yes: each of the transcript's files moves to the
    /// Trash — never deleted, and only what [`crate::sessions::forget_plan`]
    /// would still offer for it (a path that left Baaz's home, or was
    /// replaced by a link, stays).
    pub(crate) fn trash_transcript(&mut self, target: Option<String>, cx: &mut Context<Self>) {
        let Some(target) = target else { return };
        let state_dir = crate::store::support_dir();
        let homes = [
            provider_claude_code::home::home_dir(&state_dir),
            provider_codex::home::home_dir(&state_dir),
        ];
        let mut failed = None;
        for path in target.lines().map(std::path::PathBuf::from) {
            let inside = homes.iter().any(|home| path.starts_with(home));
            let real = std::fs::symlink_metadata(&path).is_ok_and(|meta| !meta.file_type().is_symlink());
            if !inside || !real {
                continue;
            }
            if let Err(error) = crate::skills::move_to_trash_dir(&path, &crate::skills::trash_dir()) {
                failed = Some(error);
            }
        }
        if let Some(error) = failed {
            self.overlays.update(cx, |overlays, cx| overlays.toast("Couldn't move the transcript", error, cx));
        }
    }

    /// Open a fork minted by `ForkSession` as a new provider-lane view on
    /// the same provider: connect a fresh child through the factory and
    /// resume the fork there, then land it like any lane open. A refused
    /// resume dialogs with the reason and opens nothing — never a silent
    /// muse fallback, never the source session.
    pub(crate) fn open_forked_on_provider(
        &mut self,
        provider_id: ProviderId,
        session_id: String,
        workspace: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A fork while a provider switch is pending cancels it; the fork
        // then opens normally instead of landing into the switch's marker.
        self.cancel_pending_switch(cx);
        self.load_menu_sources(std::path::PathBuf::from(workspace.clone()), cx);
        // A fork is an ask like any other: it joins the last-wins epoch.
        let epoch = self.open_state.begin_open();
        let mode = self.provider_open_mode(provider_id, Some(&workspace), None, cx);
        // On the scripted stand-in (`--no-connect` / `--replay`) the
        // stand-in answers `OpenSession` but no resume — the fork still
        // goes through `ResumeSession`, the same work as a live one, so the
        // failure is the honest one.
        let factory = self.providers.clone();
        let inline = factory.lands_inline();
        let workspace_bg = workspace.clone();
        // The terminal relay's route: the fork's bridge answers for the
        // fork-minted id, registered BEFORE the resume runs.
        self.register_terminal_session(
            &session_id,
            std::path::PathBuf::from(workspace.clone()),
            provider_id.as_str(),
            cx,
        );
        let registered = session_id.clone();
        let work = move || -> Result<ProviderOpen, provider::ProviderError> {
            let provider = factory.open(provider_id)?;
            #[cfg(not(all(test, feature = "probe")))]
            let (provider, events) = conn::gate(provider);
            let ack = provider.send(provider::Command::ResumeSession {
                request_id: provider::new_request_id(),
                session_id: session_id.clone(),
                cursor: None,
                metadata_only: false,
                mode: Some(mode),
                workspace: Some(workspace_bg.clone()),
                env: baaz_core::worktrees::env_for_folder(&crate::store::support_dir(), std::path::Path::new(&workspace_bg)),
            })?;
            // Tests drain after the resume, synchronously on the test
            // executor: no forwarding thread wakes the lane task.
            #[cfg(feature = "probe")]
            #[cfg(test)]
            let (provider, events) = conn::gate_sync(provider);
            match ack {
                provider::Ack::Session { session_id, title, .. } => Ok(ProviderOpen {
                    provider_id,
                    provider,
                    events,
                    session_id,
                    project: None,
                    workspace: workspace_bg,
                    title,
                    epoch,
                    lease_held: false,
                    stored_turns: None,
                    fresh_thread_model: None,
                    // The fork groups under no project: it is a new session
                    // the sidebar names when its first turn lands.
                }),
                other => Err(provider::ProviderError::Rejected {
                    reason: format!("ResumeSession answered {other:?} instead of a session"),
                }),
            }
        };
        self.run_open(inline, work, move |this, result, window, cx| match result {
            Ok(open) => {
                // As on a reopen: a resume that answers another id keeps
                // the fork's bridge reaching this session.
                if open.session_id != registered {
                    this.terminal_service.alias_session(&registered, &open.session_id, cx);
                }
                this.finish_provider_open(open, window, cx);
            }
            Err(error) => {
                if !this.open_state.is_current_open(epoch) {
                    crate::baaz_log!(
                        "provider fork resume failed for a stale ask ({}): {error}",
                        provider_id.label()
                    );
                    return;
                }
                crate::baaz_log!("provider fork resume failed ({}): {error}", provider_id.label());
                this.abandon_switch(cx);
                this.set_dialog(
                    cx,
                    Dialog {
                        title: format!("Couldn't start {}", provider_id.label()),
                        detail: error.to_string(),
                        kind: DialogKind::Error,
                        primary: "Dismiss",
                        action: DialogAction::Dismiss,
                        archive_target: None,
                    },
                );
            }
        }, window, cx);
    }
}
