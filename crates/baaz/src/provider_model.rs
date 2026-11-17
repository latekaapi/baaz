//! The provider-status model (task 4.4): what the window knows about each
//! provider, owned once and read by every surface that shows it.
//!
//! The status service (`provider_status.rs`) probes in the background and
//! writes its cache; [`ProviderStatusModel`] follows it ([`Self::sync`],
//! one cheap stamp compare per frame) and holds what the window derives
//! from it: the statuses the connect screen, the setup sheet, the
//! sidebar's footer, Settings › Providers and the import offer read; the
//! quiet per-provider sign-out banners the composer shows (D5 B5); the
//! cards waiting on a press (Re-check, Sign out); and the one Codex sign-in
//! flow, which the connect row, the Settings card and the setup sheet all
//! drive and show.
//!
//! Readers hold the entity and read it; the screens that draw it (the
//! connect screen, the setup sheet, the Settings page) observe it, so a
//! change redraws them without the root. What reaches past it — a default
//! provider that follows the probes, Muse starting once it is available —
//! goes to the root as [`ProviderModelEvent::Changed`].

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gpui::{AppContext as _, Context, Entity, EventEmitter};

use crate::connect::{
    initial_connect_statuses, run_codex_login, signed_out_banner_text, status_stamp, CodexLogin, StatusStamp,
};
use crate::overlays::Overlays;
use crate::provider_status::{Headline, ProviderStatus};
use crate::providers::ProviderId;
use crate::settings_providers::{settled_toast, ProviderBusy};

/// How long a settled press's toast stays: it is a press's answer, not news
/// (QA, 2026-10-30: it sat on screen through several more steps).
const SETTLED_TOAST_FOR: std::time::Duration = std::time::Duration::from_secs(5);

/// What the model tells the root.
#[derive(Clone, Debug)]
pub(crate) enum ProviderModelEvent {
    /// The statuses changed in a way a person can see: the root follows
    /// (the default provider, Muse starting, a redraw).
    Changed,
}

/// The window's provider facts (see the module doc).
pub(crate) struct ProviderStatusModel {
    /// The statuses every surface renders: the cache (or the script) at
    /// boot, kept fresh from the status service.
    pub(crate) statuses: Vec<ProviderStatus>,
    /// The cache's mtime and the service's publish count when last read,
    /// so a frame compares a stamp instead of reading the cache.
    stamp: StatusStamp,
    /// Quiet per-provider banners for providers that later probe Signed
    /// out: shown on that provider's composer, never a gate.
    pub(crate) banners: HashMap<ProviderId, String>,
    /// Banners a typed auth failure raised (D5 B5): a re-probe that still
    /// reads Connected (an invalid API key reads "logged in") must not clear
    /// them; the person's own Sign in hands them back to the status.
    typed_signed_out: HashSet<ProviderId>,
    /// Providers the cache has called Connected this launch: only these
    /// earn a sign-out banner (a provider never connected gets none).
    seen_connected: HashSet<ProviderId>,
    /// Provider cards waiting on something the person pressed (Re-check,
    /// Sign out): the card says so until the result lands, then a toast
    /// names it — a press with no visible answer read as broken.
    pub(crate) busy: HashMap<ProviderId, ProviderBusy>,
    /// The Codex Sign in flow's state (Idle until a row or card starts it).
    pub(crate) codex_login: CodexLogin,
    /// Cancel for the running Codex login thread, replaced on every start.
    codex_cancel: Arc<AtomicBool>,
    /// Bumped by every Codex sign-in start: a superseded flow's updates are
    /// dropped (Use a code instead after the browser flow).
    codex_generation: u64,
    /// The window's floating layer, for the settled-press toasts.
    overlays: Entity<Overlays>,
}

impl EventEmitter<ProviderModelEvent> for ProviderStatusModel {}

impl ProviderStatusModel {
    /// The model at boot: statuses from the cache (or the script), stamped.
    pub(crate) fn new(overlays: Entity<Overlays>) -> Self {
        ProviderStatusModel {
            statuses: initial_connect_statuses(),
            stamp: status_stamp(),
            banners: HashMap::new(),
            typed_signed_out: HashSet::new(),
            seen_connected: HashSet::new(),
            busy: HashMap::new(),
            codex_login: CodexLogin::Idle,
            codex_cancel: Arc::new(AtomicBool::new(false)),
            codex_generation: 0,
            overlays,
        }
    }

    /// `id`'s status as the window shows it, Checking until a probe lands.
    pub(crate) fn status(&self, id: ProviderId) -> ProviderStatus {
        self.statuses.iter().find(|status| status.provider == id).cloned().unwrap_or_else(|| ProviderStatus::checking(id))
    }

    /// Re-read the statuses at once (the connect screen opening, a login
    /// sample): the rows start from the cache, stamped. Its callers are the
    /// root's, which redraws.
    pub(crate) fn reload(&mut self) {
        self.statuses = initial_connect_statuses();
        self.stamp = status_stamp();
    }

    /// Follow the status cache: background probes only write the cache
    /// file, so its mtime (or the scripted source, in deterministic mode)
    /// is the clock. A visible change re-derives the banners, notifies the
    /// observers and tells the root ([`ProviderModelEvent::Changed`]).
    pub(crate) fn sync(&mut self, cx: &mut Context<Self>) {
        if crate::provider_status::deterministic() {
            let fresh = initial_connect_statuses();
            if fresh != self.statuses {
                self.statuses = fresh;
                self.sync_banners();
                cx.notify();
                cx.emit(ProviderModelEvent::Changed);
            }
            return;
        }
        let stamp = status_stamp();
        if stamp == self.stamp {
            return;
        }
        self.stamp = stamp;
        // Before the no-visible-change return: a Re-check that found what
        // the card already showed still owes the person its answer.
        self.settle_busy(cx);
        let fresh = initial_connect_statuses();
        // A re-probe that found what the window already shows moves only
        // `checked_at`: nothing to redraw (nothing renders at rest, P0).
        if visible(&fresh) == visible(&self.statuses) {
            self.statuses = fresh;
            return;
        }
        self.statuses = fresh;
        self.sync_banners();
        cx.notify();
        cx.emit(ProviderModelEvent::Changed);
    }

    /// The quiet banners: a provider the cache once called Connected that
    /// now reads Signed out gets "X is signed out — Sign in" on its
    /// composer; Connected clears it. Never a full-screen gate.
    fn sync_banners(&mut self) {
        for status in &self.statuses {
            let id = status.provider;
            match status.headline() {
                Headline::Connected => {
                    self.seen_connected.insert(id);
                    if !self.typed_signed_out.contains(&id) {
                        self.banners.remove(&id);
                    }
                    if id == ProviderId::Codex && matches!(self.codex_login, CodexLogin::Completed) {
                        self.codex_login = CodexLogin::Idle;
                    }
                }
                Headline::SignedOut if self.seen_connected.contains(&id) => {
                    self.banners.insert(id, signed_out_banner_text(id));
                }
                _ => {}
            }
        }
    }

    /// A typed credential failure (D5 B5, the `signed-out` step verb): the
    /// banner on `id`'s composer, kept through re-probes that still read
    /// Connected until the person signs in. The banner is the composer's,
    /// which the root draws: its callers redraw the root.
    pub(crate) fn raise_typed_signed_out(&mut self, id: ProviderId) {
        self.banners.insert(id, signed_out_banner_text(id));
        self.typed_signed_out.insert(id);
    }

    /// The person acts on a banner (its Sign in): from here the status
    /// decides when the banner goes (the next probe that reads Connected).
    pub(crate) fn release_typed_signed_out(&mut self, id: ProviderId) {
        self.typed_signed_out.remove(&id);
    }

    /// Muse signed in over its own connection: its banner goes, and it
    /// counts as seen, so a later sign-out earns the banner, not a gate.
    /// (The root, which draws the banner, redraws after.)
    pub(crate) fn muse_signed_in(&mut self) {
        self.banners.remove(&ProviderId::Muse);
        self.seen_connected.insert(ProviderId::Muse);
    }

    /// Muse signed out in the shell: its quiet banner (the root redraws).
    pub(crate) fn muse_signed_out(&mut self) {
        self.seen_connected.insert(ProviderId::Muse);
        self.banners.insert(ProviderId::Muse, signed_out_banner_text(ProviderId::Muse));
    }

    // ── presses that wait on a probe ──────────────────────────────────

    /// Re-probe `id` with its card saying so; [`Self::settle_busy`] clears
    /// it and names the result once the probe lands.
    pub(crate) fn recheck_with_feedback(&mut self, id: ProviderId, after_sign_out: bool, cx: &mut Context<Self>) {
        if crate::provider_status::deterministic() {
            // Captures probe nothing: the scripted status is the answer.
            crate::provider_status::recheck_provider(id);
            cx.notify();
            return;
        }
        self.busy.insert(id, ProviderBusy::Checking { after_sign_out });
        crate::provider_status::recheck_provider(id);
        cx.notify();
    }

    /// A confirmed sign-out is running: the card says "Signing out…".
    pub(crate) fn mark_signing_out(&mut self, id: ProviderId, cx: &mut Context<Self>) {
        self.busy.insert(id, ProviderBusy::SigningOut);
        cx.notify();
    }

    /// A sign-out failed: the card stops waiting and a toast says why
    /// (owner, 2026-10-29: a failure is never only a log line).
    pub(crate) fn sign_out_failed(&mut self, id: ProviderId, error: String, cx: &mut Context<Self>) {
        self.busy.remove(&id);
        self.overlays.update(cx, |overlays, _| {
            overlays.toast(format!("Couldn't sign out of {}", id.label()), error);
        });
        cx.notify();
    }

    /// A probe landed: every card waiting on a re-check whose probe is no
    /// longer running settles, with a toast naming what it found.
    fn settle_busy(&mut self, cx: &mut Context<Self>) {
        let settled: Vec<(ProviderId, bool)> = self
            .busy
            .iter()
            .filter_map(|(id, busy)| match busy {
                ProviderBusy::Checking { after_sign_out } if !crate::provider_status::probing(*id) => {
                    Some((*id, *after_sign_out))
                }
                _ => None,
            })
            .collect();
        if settled.is_empty() {
            return;
        }
        let statuses = crate::provider_status::live_statuses();
        for (id, after_sign_out) in settled {
            self.busy.remove(&id);
            if let Some(status) = statuses.iter().find(|status| status.provider == id) {
                let (title, body) = settled_toast(status, after_sign_out);
                let toast = self.overlays.update(cx, |overlays, _| overlays.toast(title, body));
                // A press's answer, not news: it leaves by itself. A failed
                // sign-out's toast is not this one and stays.
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(SETTLED_TOAST_FOR).await;
                    let _ = this.update(cx, |this, cx| {
                        this.overlays.update(cx, |overlays, cx| {
                            overlays.dismiss_toast(&toast);
                            cx.notify();
                        });
                    });
                })
                .detach();
            }
        }
        cx.notify();
    }

    // ── the Codex sign-in ─────────────────────────────────────────────

    /// Start Codex's sign-in unless one already waits on the person — the
    /// connect row, the Settings card, the setup sheet's next step and the
    /// composer banner all come here.
    pub(crate) fn begin_codex_signin(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.codex_login, CodexLogin::Waiting { .. } | CodexLogin::Device { .. }) {
            self.start_codex_login(false, cx);
        }
    }

    /// Cancel the running Codex sign-in: its thread sees the flag and ends.
    pub(crate) fn cancel_codex_login(&mut self) {
        self.codex_cancel.store(true, Ordering::SeqCst);
    }

    /// The muted line under the rows while Codex signs in (D5 B3): "Browser
    /// didn't open? Copy the link · Use a code instead" while the browser
    /// flow waits; "Enter <code> at <host/path> · Copy code · Open page"
    /// once the person asked for a code (owner 2026-10-24: the code goes in
    /// this line, no new surface). Empty otherwise.
    pub(crate) fn codex_assist(&self) -> Vec<aui::screens::AssistRun> {
        crate::connect::codex_assist_runs(&self.codex_login)
    }

    /// A link in the assist line (the connect screen's, or the Codex card's
    /// in Settings): copy the browser link, switch to a device code, copy the
    /// code, open the page the code goes in.
    pub(crate) fn codex_assist_link(&mut self, key: &str, cx: &mut Context<Self>) {
        match (key, &self.codex_login) {
            ("copy-link", CodexLogin::Waiting { auth_url }) if !auth_url.is_empty() => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(auth_url.clone()));
            }
            ("use-code", CodexLogin::Waiting { .. }) => self.start_codex_login(true, cx),
            ("copy-code", CodexLogin::Device { code, .. }) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(code.clone()));
            }
            ("open-page", CodexLogin::Device { url, .. }) => {
                let _ = crate::auth::open_in_browser(url);
            }
            _ => {}
        }
    }

    /// `codex-login:<waiting|device|failed>` (`--steps`): Codex's sign-in in
    /// that state with sample values, for captures. Starts nothing. Any
    /// other state is an `Err` and leaves the sign-in as it was.
    pub(crate) fn step_codex_login(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        self.codex_login = match rest.trim() {
            "waiting" => CodexLogin::Waiting { auth_url: "https://example.invalid/oauth/authorize".into() },
            "device" => CodexLogin::Device { url: "https://auth.openai.com/codex/device".into(), code: "K7QX-9P2M".into() },
            "failed" => CodexLogin::Failed { reason: "The sign-in timed out.".into() },
            other => return Err(format!("unknown codex-login state {other:?} (expected waiting, device or failed)")),
        };
        cx.notify();
        Ok(())
    }

    /// Start the Codex Sign in flow: the Waiting row applies at once, the
    /// blocking app-server login runs off the UI thread, its start answer
    /// (the browser link, or the device code) reaches the row as it lands,
    /// and its terminal state re-probes Codex on success. `device` is "Use a
    /// code instead". A newer start (Use a code after the browser flow)
    /// supersedes the older one: its updates carry an older generation and
    /// are dropped.
    pub(crate) fn start_codex_login(&mut self, device: bool, cx: &mut Context<Self>) {
        let program = crate::providers::resolve_program(ProviderId::Codex)
            .map(|program| program.to_string_lossy().into_owned());
        let Some(program) = program else {
            self.codex_login = CodexLogin::Failed { reason: "Codex is not installed — Install first.".into() };
            cx.notify();
            return;
        };
        // A running flow is abandoned: its thread sees the old flag.
        self.codex_cancel.store(true, Ordering::SeqCst);
        self.codex_cancel = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&self.codex_cancel);
        self.codex_generation += 1;
        let generation = self.codex_generation;
        self.codex_login = CodexLogin::Waiting { auth_url: String::new() };
        cx.notify();
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<CodexLogin>();
        cx.background_spawn(async move {
            let progress = tx.clone();
            let terminal = run_codex_login(&program, &cancel, device, move |begun| {
                let _ = progress.unbounded_send(begun.clone());
            });
            let _ = tx.unbounded_send(terminal);
        })
        .detach();
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            while let Some(state) = rx.next().await {
                let alive = this.update(cx, |this, cx| {
                    if this.codex_generation != generation {
                        return;
                    }
                    if state == CodexLogin::Completed {
                        crate::provider_status::reprobe_provider(ProviderId::Codex);
                    }
                    this.codex_login = state;
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }
}

/// The part of the statuses a person can see: all of it but when each was
/// last checked — except whether it has been checked at all (Checking).
fn visible(statuses: &[ProviderStatus]) -> Vec<ProviderStatus> {
    statuses
        .iter()
        .cloned()
        .map(|mut status| {
            status.checked_at = status.checked_at.map(|_| 0);
            status
        })
        .collect()
}
