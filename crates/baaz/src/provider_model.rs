//! The provider-status model (task 4.4): what the window knows about each
//! provider, owned once and read by every surface that shows it.
//!
//! The status service (`provider_status.rs`) probes in the background and
//! writes its cache; [`ProviderStatusModel`] follows it ([`Self::sync`],
//! one cheap stamp compare per frame) and holds what the window derives
//! from it: the statuses the connect screen, the setup sheet, the
//! sidebar's footer, Settings › Providers and the import offer read; the
//! provider new sessions start on, which follows the statuses until the
//! person picks one ([`NewProvider`]); the
//! quiet per-provider sign-out banners the composer shows (D5 B5); the
//! cards waiting on a press (Re-check, Sign out); and the one Codex sign-in
//! flow, which the connect row, the Settings card and the setup sheet all
//! drive and show.
//!
//! Readers hold the entity and read it; the screens that draw it (the
//! connect screen, the setup sheet, the Settings page) observe it, so a
//! change redraws them without the root. What reaches past it — Muse
//! starting once it is available, the root's redraw — goes to the root as
//! [`ProviderModelEvent::Changed`].

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
    /// (Muse starting, a redraw).
    Changed,
}

/// The provider new sessions start on (the composer's switcher, the
/// footer's "New sessions start in"), and whose choice it is.
pub(crate) struct NewProvider {
    /// Its registry key. `--provider` may name a key no registration
    /// answers to; a new session then refuses rather than guessing.
    key: String,
    /// The person chose it — `--provider`, a pick an earlier run stored, a
    /// pick this run — so the statuses never re-pick it for them. Kept here
    /// as well as in the store: a pick whose write failed still holds.
    chosen: bool,
}

impl NewProvider {
    /// At boot, over the boot statuses (the cache, never a probe): the
    /// `--provider` key, else the stored pick while it can start a session,
    /// else the first in the one provider order that can run
    /// ([`crate::settings_providers::default_new_provider`]) — never a
    /// provider for being itself (docs/50 P1).
    pub(crate) fn at_boot(explicit: Option<String>, statuses: &[ProviderStatus]) -> Self {
        Self::at_boot_with(explicit, crate::providers::read_last_provider(), statuses)
    }

    /// [`Self::at_boot`] with the stored pick handed in.
    fn at_boot_with(explicit: Option<String>, stored: Option<ProviderId>, statuses: &[ProviderStatus]) -> Self {
        if let Some(key) = explicit {
            return NewProvider { key, chosen: true };
        }
        let id = stored
            .filter(|id| crate::settings_providers::usable_provider(statuses, *id))
            .or_else(|| crate::settings_providers::default_new_provider(statuses))
            .unwrap_or_else(ProviderId::first);
        NewProvider { key: id.as_str().to_owned(), chosen: stored.is_some() }
    }

    /// The person picked `id`.
    fn choose(&mut self, id: ProviderId) {
        *self = NewProvider { key: id.as_str().to_owned(), chosen: true };
    }

    /// With no pick of the person's, the default follows `statuses` as they
    /// land: boot chose it over the cache, or over Checking rows on a first
    /// launch, which say nothing yet. The first that can run in the one
    /// provider order (Gate 4: a first launch stayed on Muse, then first in
    /// the registry, with every provider connected). A pick is never re-picked.
    fn follow(&mut self, statuses: &[ProviderStatus]) {
        if self.chosen {
            return;
        }
        if let Some(id) = crate::settings_providers::default_new_provider(statuses) {
            self.key = id.as_str().to_owned();
        }
    }
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
    /// Where new sessions start ([`NewProvider`]).
    new_provider: NewProvider,
}

impl EventEmitter<ProviderModelEvent> for ProviderStatusModel {}

impl ProviderStatusModel {
    /// The model at boot: statuses from the cache (or the script), stamped,
    /// and the new-session provider over them (`explicit`: `--provider`).
    pub(crate) fn new(overlays: Entity<Overlays>, explicit: Option<String>) -> Self {
        let statuses = initial_connect_statuses();
        let new_provider = NewProvider::at_boot(explicit, &statuses);
        ProviderStatusModel {
            statuses,
            stamp: status_stamp(),
            banners: HashMap::new(),
            typed_signed_out: HashSet::new(),
            seen_connected: HashSet::new(),
            busy: HashMap::new(),
            codex_login: CodexLogin::Idle,
            codex_cancel: Arc::new(AtomicBool::new(false)),
            codex_generation: 0,
            overlays,
            new_provider,
        }
    }

    /// The new-session provider's key, as the switcher holds it.
    pub(crate) fn new_provider(&self) -> &str {
        &self.new_provider.key
    }

    /// The new-session provider as a registry entry; `None` only for a
    /// `--provider` key no registration answers to.
    pub(crate) fn new_provider_id(&self) -> Option<ProviderId> {
        ProviderId::resolve(&self.new_provider.key).ok()
    }

    /// The person picked `id` (the switcher, the footer's or Settings'
    /// default, a fallback they caused): it holds for this run and is
    /// stored for the next. The statuses never re-pick it.
    pub(crate) fn choose_new_provider(&mut self, id: ProviderId) {
        self.new_provider.choose(id);
        crate::providers::write_last_provider(id);
    }

    /// Point new sessions at `id` for the next start without making it the
    /// person's pick (the Skills page's "write one" runs on the provider
    /// that manages skills, whatever the switcher said).
    pub(crate) fn point_new_provider(&mut self, id: ProviderId) {
        self.new_provider.key = id.as_str().to_owned();
    }


    /// `id`'s status as the window shows it, Checking until a probe lands.
    pub(crate) fn status(&self, id: ProviderId) -> ProviderStatus {
        self.statuses.iter().find(|status| status.provider == id).cloned().unwrap_or_else(|| ProviderStatus::checking(id))
    }

    /// Re-read the statuses at once (the connect screen opening, a login
    /// sample): the rows start from the cache, stamped. Its callers are the
    /// root's, which redraws.
    pub(crate) fn reload(&mut self, cx: &mut gpui::App) {
        self.statuses = initial_connect_statuses();
        crate::skills_page::note_statuses(&self.statuses, cx);
        self.stamp = status_stamp();
    }

    /// Follow the status cache: background probes only write the cache
    /// file, so its mtime (or the scripted source, in deterministic mode)
    /// is the clock. A visible change re-derives the banners, notifies the
    /// observers and tells the root ([`ProviderModelEvent::Changed`]).
    pub(crate) fn sync(&mut self, cx: &mut Context<Self>) {
        let fresh = if crate::runtime::providers_scripted() {
            let fresh = initial_connect_statuses();
            if fresh == self.statuses {
                return;
            }
            fresh
        } else {
            let stamp = status_stamp();
            if stamp == self.stamp {
                return;
            }
            self.stamp = stamp;
            // Before the no-visible-change return: a Re-check that found
            // what the card already showed still owes the person its answer.
            self.settle_busy(cx);
            let fresh = initial_connect_statuses();
            // A re-probe that found what the window already shows moves
            // only `checked_at`: nothing to redraw (nothing renders at
            // rest, P0).
            if visible(&fresh) == visible(&self.statuses) {
                self.statuses = fresh;
                return;
            }
            fresh
        };
        self.statuses = fresh;
        record_accounts(&self.statuses);
        crate::skills_page::note_statuses(&self.statuses, cx);
        self.sync_banners();
        self.new_provider.follow(&self.statuses);
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
    /// which the root draws from this model: the notify redraws it.
    pub(crate) fn raise_typed_signed_out(&mut self, id: ProviderId, cx: &mut Context<Self>) {
        self.banners.insert(id, signed_out_banner_text(id));
        self.typed_signed_out.insert(id);
        cx.notify();
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
        let Some(id) = ProviderId::with(provider::ProviderTrait::AccountOnConnection) else { return };
        self.banners.remove(&id);
        self.seen_connected.insert(id);
    }

    /// Muse signed out in the shell: its quiet banner (the root redraws).
    pub(crate) fn muse_signed_out(&mut self) {
        let Some(id) = ProviderId::with(provider::ProviderTrait::AccountOnConnection) else { return };
        self.seen_connected.insert(id);
        self.banners.insert(id, signed_out_banner_text(id));
    }

    // ── presses that wait on a probe ──────────────────────────────────

    /// Re-probe `id` with its card saying so; [`Self::settle_busy`] clears
    /// it and names the result once the probe lands.
    pub(crate) fn recheck_with_feedback(&mut self, id: ProviderId, after_sign_out: bool, cx: &mut Context<Self>) {
        // Captures probe nothing: the scripted status is the answer, and no
        // card waits on it.
        if !crate::runtime::providers_scripted() {
            self.busy.insert(id, ProviderBusy::Checking { after_sign_out });
        }
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
        self.overlays.update(cx, |overlays, cx| {
            overlays.toast(format!("Couldn't sign out of {}", id.label()), error, cx);
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
                let toast = self.overlays.update(cx, |overlays, cx| overlays.toast(title, body, cx));
                // A press's answer, not news: it leaves by itself. A failed
                // sign-out's toast is not this one and stays.
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(SETTLED_TOAST_FOR).await;
                    let _ = this.update(cx, |this, cx| {
                        this.overlays.update(cx, |overlays, cx| {
                            overlays.dismiss_toast(&toast, cx);
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

    #[cfg(feature = "probe")]
    /// `codex-login:<waiting|device|failed>` (`--steps`): Codex's sign-in in
    /// that state with sample values, for captures. Starts nothing. Any
    /// other state is an `Err` and leaves the sign-in as it was.
    pub(crate) fn step_codex_login(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let state = match rest.trim() {
            "waiting" => CodexLogin::Waiting { auth_url: "https://example.invalid/oauth/authorize".into() },
            "device" => CodexLogin::Device { url: "https://auth.openai.com/codex/device".into(), code: "K7QX-9P2M".into() },
            "failed" => CodexLogin::Failed { reason: "The sign-in timed out.".into() },
            other => return Err(format!("unknown codex-login state {other:?} (expected waiting, device or failed)")),
        };
        self.set_codex_login(state, cx);
        Ok(())
    }

    /// Move Codex's sign-in to `state` (the row, the card and the assist
    /// line draw it), redrawn when it moved.
    fn set_codex_login(&mut self, state: CodexLogin, cx: &mut Context<Self>) {
        if self.codex_login != state {
            self.codex_login = state;
            cx.notify();
        }
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
            self.set_codex_login(CodexLogin::Failed { reason: "Not installed — Install first.".into() }, cx);
            return;
        };
        // A running flow is abandoned: its thread sees the old flag.
        self.codex_cancel.store(true, Ordering::SeqCst);
        self.codex_cancel = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&self.codex_cancel);
        self.codex_generation += 1;
        let generation = self.codex_generation;
        self.set_codex_login(CodexLogin::Waiting { auth_url: String::new() }, cx);
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
                    this.set_codex_login(state, cx);
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

/// docs/85 16.1: every signed-in account the statuses name goes into
/// baaz.db's `account` table (its email, how it signed in, its plan),
/// off the UI thread; never in a capture or with scripted providers.
fn record_accounts(statuses: &[ProviderStatus]) {
    if crate::runtime::captures() || crate::runtime::providers_scripted() || cfg!(test) {
        return;
    }
    let now = baaz_core::clock::now_ms() as i64;
    let accounts: Vec<baaz_core::db::accounts::Account> = statuses
        .iter()
        .filter_map(|s| match &s.auth {
            crate::provider_status::Auth::SignedIn { email, plan, method, .. } => Some(baaz_core::db::accounts::Account {
                provider: s.provider.as_str().to_owned(),
                label: email.clone(),
                lane: method.clone(),
                plan: plan.clone().or_else(|| s.usage.as_ref().and_then(|u| u.plan.clone())),
                observed_at: s.checked_at.unwrap_or(now),
            }),
            _ => None,
        })
        .collect();
    if accounts.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        crate::db::call(crate::db::path(), move |opened| {
            for account in &accounts {
                if let Err(why) = baaz_core::db::accounts::record(&opened.conn, account) {
                    crate::baaz_log!("account: not recorded ({why})");
                }
            }
        });
    });
}


#[cfg(feature = "probe")]
#[cfg(test)]
mod tests {
    use super::NewProvider;
    use crate::provider_status::{Auth, Installed, ProviderStatus};
    use crate::providers::ProviderId;

    fn status(id: ProviderId, connected: bool) -> ProviderStatus {
        let mut status = ProviderStatus::checking(id);
        // Probed: a row never checked reads Checking, never Connected.
        status.checked_at = Some(1);
        if connected {
            status.installed = Installed::yes("1.0", "/bin/x");
            status.auth = Auth::SignedIn { email: Some("a@b".into()), plan: None, method: None, source: None };
        }
        status
    }

    fn checking() -> Vec<ProviderStatus> {
        ProviderId::all().into_iter().map(ProviderStatus::checking).collect()
    }

    fn all_connected() -> Vec<ProviderStatus> {
        ProviderId::all().into_iter().map(|id| status(id, true)).collect()
    }

    /// Gate 4: a first launch (no cache, nothing stored) started new
    /// sessions on Muse, then first in the registry. Boot over Checking
    /// rows takes the one order's first; once the statuses land, the first
    /// connected in the one order — not Muse with everything connected.
    #[test]
    fn a_first_launch_follows_the_statuses_to_the_first_connected() {
        let mut new = NewProvider::at_boot_with(None, None, &checking());
        assert_eq!(new.key, ProviderId::first().as_str());
        assert_ne!(new.key, ProviderId::Muse.as_str(), "Muse is no default for being Muse");
        new.follow(&all_connected());
        assert_eq!(new.key, ProviderId::all()[0].as_str(), "the first connected in the one order");
        assert_eq!(new.key, ProviderId::ClaudeCode.as_str());
        // Only Codex connected: Codex.
        let statuses = vec![status(ProviderId::Muse, false), status(ProviderId::ClaudeCode, false), status(ProviderId::Codex, true)];
        new.follow(&statuses);
        assert_eq!(new.key, ProviderId::Codex.as_str());
        // Only Muse connected: Muse, on the same rule as any other.
        let statuses = vec![status(ProviderId::Muse, true), status(ProviderId::ClaudeCode, false), status(ProviderId::Codex, false)];
        new.follow(&statuses);
        assert_eq!(new.key, ProviderId::Muse.as_str());
    }

    /// A pick stored by an earlier run is never re-picked by the statuses.
    #[test]
    fn a_stored_pick_is_never_followed_away() {
        let mut new = NewProvider::at_boot_with(None, Some(ProviderId::Muse), &all_connected());
        assert_eq!(new.key, ProviderId::Muse.as_str());
        new.follow(&all_connected());
        assert_eq!(new.key, ProviderId::Muse.as_str(), "the statuses overrode a stored pick");
    }

    /// A pick made this run holds, whether or not its store write landed.
    #[test]
    fn a_pick_this_run_is_never_followed_away() {
        let mut new = NewProvider::at_boot_with(None, None, &checking());
        new.choose(ProviderId::Codex);
        new.follow(&all_connected());
        assert_eq!(new.key, ProviderId::Codex.as_str(), "the statuses overrode this run's pick");
    }

    /// `--provider` is the person's pick too, even one the registry refuses.
    #[test]
    fn the_flag_is_never_followed_away() {
        let mut new = NewProvider::at_boot_with(Some("codex".into()), None, &checking());
        new.follow(&all_connected());
        assert_eq!(new.key, "codex");
        let mut unknown = NewProvider::at_boot_with(Some("nope".into()), None, &checking());
        unknown.follow(&all_connected());
        assert_eq!(unknown.key, "nope", "an unknown key is refused at the open, not replaced");
    }
}
