//! A session's turn checkpoints (docs/82 6.3), the runtime half: when a
//! live turn starts its folder is snapshotted to `refs/baaz/<uuid>/pre/<turn>`,
//! when it ends (done, stopped, failed or its child lost) to
//! `refs/baaz/<uuid>/<turn>`. The git work and the bookkeeping are
//! [`crate::checkpoints`]; here it runs off the UI thread one job at a time
//! and the application hears [`SessionEvent::CheckpointTaken`] when a turn's
//! refs landed, so an open Changes screen can offer its scopes.
//!
//! Off for: a session with no folder, a capture (its scripted turns run in
//! whatever repository the probe was pointed at, which is the owner's), a
//! replayed or read-only lane, and — in tests — any folder outside the
//! temp dir, so a test never writes refs into the repository it runs in.

use super::*;

/// The folder a session in `workspace` checkpoints, if any (see the module
/// doc).
pub(super) fn checkpoint_folder(workspace: &str) -> Option<std::path::PathBuf> {
    if workspace.is_empty() || crate::runtime::captures() {
        return None;
    }
    let folder = std::path::PathBuf::from(workspace);
    if cfg!(test) {
        let temp = std::env::temp_dir();
        let temp = temp.canonicalize().unwrap_or(temp);
        let real = folder.canonicalize().unwrap_or_else(|_| folder.clone());
        if !real.starts_with(&temp) {
            return None;
        }
    }
    Some(folder)
}

impl SessionRuntime {
    /// The live turn `turn_id` started running: its pre-turn checkpoint.
    pub(super) fn checkpoint_turn_started(&mut self, turn_id: &str, cx: &mut Context<Self>) {
        if self.replay || self.read_only {
            return;
        }
        if let Some(job) = self.checkpoints.turn_started(turn_id) {
            self.run_checkpoint_job(job, cx);
        }
    }

    /// The running turn is ending (any outcome): its checkpoint. Called
    /// before the running turn is cleared; nothing running, nothing taken.
    pub(super) fn checkpoint_running_ended(&mut self, cx: &mut Context<Self>) {
        let Some(turn_id) = self.running.as_ref().map(|running| running.turn_id.clone()) else { return };
        if let Some(job) = self.checkpoints.turn_ended(&turn_id) {
            self.run_checkpoint_job(job, cx);
        }
    }

    /// Run one job in the background; the next waits for it.
    fn run_checkpoint_job(&mut self, job: crate::checkpoints::Job, cx: &mut Context<Self>) {
        let (provider, id) = (self.provider_kind().as_str().to_owned(), self.session_id.clone());
        cx.spawn(async move |this, cx| {
            let done = cx
                .background_spawn(async move {
                    crate::checkpoints::run_job(job, move || {
                        let state = crate::store::support_dir();
                        crate::db::read(crate::db::path_in(&state), move |db| crate::db::sessions::read_row(&db.conn, &provider, &id).ok().flatten())
                            .flatten()
                    })
                })
                .await;
            let _ = this.update(cx, |this: &mut Self, cx| {
                let landed = !done.written.is_empty();
                if let Some(next) = this.checkpoints.job_done(done) {
                    this.run_checkpoint_job(next, cx);
                }
                if landed {
                    cx.emit(super::SessionEvent::CheckpointTaken { session_id: this.session_id.clone() });
                }
            });
        })
        .detach();
    }

    /// Whether no checkpoint is being taken or waits to be (tests).
    #[cfg(test)]
    pub(crate) fn checkpoints_idle(&self) -> bool {
        self.checkpoints.idle()
    }

    /// Fold one provider event as the lane would (tests): a turn's start
    /// and end driven by hand, so a test can write files between them.
    #[cfg(test)]
    pub(crate) fn test_provider_event(&mut self, event: provider::ProviderEvent, cx: &mut Context<Self>) {
        self.on_provider_event(event, cx);
    }
}
