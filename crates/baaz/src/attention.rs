//! Which agent needs the person (task 3.3): macOS notifications when a
//! session moves to needing them, finishing unread or failing, and the Dock
//! badge counting the sessions that need them.
//!
//! Everything reads [`SessionEntry::session_state`] (task 3.1); nothing here
//! decides a state of its own. [`transitions`] is the pure part — what
//! changed since the last look and what to say about it — and
//! [`Harness::check_attention`] applies it: it posts through gpui's system
//! notifications (which do nothing outside an app bundle; permission is
//! asked at launch by [`ask_permission`], because a notification posted
//! before the answer is dropped) and sets the Dock tile's
//! badge. It runs after every session or provider event that can move a
//! row, gated on the list epoch, so it costs a compare when nothing moved —
//! never from render: gpui-pre 0.3.12 paints no frame for a covered window,
//! which is exactly when a notification matters.
//!
//! The last-seen states live in a gpui global ([`Notifier`]), not on
//! [`Harness`] (docs/82 §1 rule 9).

use std::collections::HashMap;

use gpui::{App, Context, Global, SharedString, SystemNotification};

use crate::app::Harness;
use crate::sidebar::{NeedKind, SessionEntry, SessionState};

/// What the person asked to hear about (Settings › Notifications, task
/// 3.5). Every event on, the badge on, the sound off by default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NotifyPrefs {
    /// A session moved to needing the person (approval or question).
    pub needs_you: bool,
    /// A turn finished while nobody was reading it.
    pub done: bool,
    /// A turn ended in an error.
    pub failed: bool,
    /// Play the system sound with each notification.
    pub sound: bool,
    /// Show the needs-you count on the Dock icon.
    pub badge: bool,
}

impl Default for NotifyPrefs {
    fn default() -> Self {
        Self { needs_you: true, done: true, failed: true, sound: false, badge: true }
    }
}

/// One notification to post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Post {
    /// The session it is about: the notification's tag, so a newer one for
    /// the same session replaces the older, and a click opens it.
    pub session_id: String,
    /// The session's name.
    pub title: String,
    /// What happened, in a few words. Never a provider name (rule 14).
    pub body: String,
}

/// What [`transitions`] found.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Look {
    /// Notifications to post, in row order.
    pub posts: Vec<Post>,
    /// Each counted session's state now: the next look's "before".
    pub states: HashMap<String, SessionState>,
    /// How many sessions need the person: the Dock badge.
    pub needs_you: usize,
}

/// The rows a person can see: not hidden, archived or a throwaway side
/// session. Only these are counted or announced.
fn counted(row: &SessionEntry) -> bool {
    !row.hidden && !row.archived && !row.side_marker
}

/// Compare every counted row's state with `before` and say what to post.
///
/// A session posts when it moves **to** needing the person (from anything
/// else), or **from** working or needing the person **to** finished-unread
/// or failed. A row seen for the first time posts nothing — it is how the
/// first look after launch learns the states without announcing them all.
/// Nothing is posted for the session in front while the app is active:
/// the person is looking at it.
pub(crate) fn transitions<'a>(
    before: &HashMap<String, SessionState>,
    rows: impl IntoIterator<Item = &'a SessionEntry>,
    front: Option<&str>,
    app_active: bool,
    prefs: NotifyPrefs,
) -> Look {
    let mut look = Look::default();
    for row in rows.into_iter().filter(|row| counted(row)) {
        let now = row.session_state();
        if matches!(now, SessionState::NeedsYou(_)) {
            look.needs_you += 1;
        }
        look.states.insert(row.id.clone(), now);
        let Some(&was) = before.get(&row.id) else { continue };
        if was == now {
            continue;
        }
        let was_live = matches!(was, SessionState::Working | SessionState::NeedsYou(_));
        let body = match now {
            SessionState::NeedsYou(kind) if !matches!(was, SessionState::NeedsYou(_)) && prefs.needs_you => match kind {
                NeedKind::Approval => "Waiting for your approval".to_owned(),
                NeedKind::Question => row
                    .pending_question
                    .as_deref()
                    .map(str::trim)
                    .filter(|q| !q.is_empty())
                    .map(|q| format!("Asks: {q}"))
                    .unwrap_or_else(|| "Has a question for you".to_owned()),
            },
            SessionState::DoneUnread if was_live && prefs.done => "Finished".to_owned(),
            SessionState::Failed if was_live && prefs.failed => "Stopped with an error".to_owned(),
            _ => continue,
        };
        if app_active && front == Some(row.id.as_str()) {
            continue;
        }
        look.posts.push(Post { session_id: row.id.clone(), title: row.label.clone(), body });
    }
    look
}

/// The sessions that need the person, in the order of `rows`: what the
/// "Needs you" filter shows and what ⌘⇧J walks.
pub(crate) fn waiting<'a>(rows: impl IntoIterator<Item = &'a SessionEntry>) -> Vec<&'a SessionEntry> {
    rows.into_iter().filter(|row| counted(row) && matches!(row.session_state(), SessionState::NeedsYou(_))).collect()
}

/// The waiting session after `current` in `order`, wrapping at the end;
/// the first one when `current` is not waiting (or nothing is open).
pub(crate) fn next_waiting(order: &[String], current: Option<&str>) -> Option<String> {
    let first = order.first()?;
    let at = current.and_then(|id| order.iter().position(|w| w == id));
    Some(match at {
        Some(i) => order[(i + 1) % order.len()].clone(),
        None => first.clone(),
    })
}

/// The last look: each session's state then, the list epoch it was taken
/// at, and the badge last set.
#[derive(Default)]
pub(crate) struct Notifier {
    states: HashMap<String, SessionState>,
    epoch: Option<u64>,
    badge: Option<usize>,
    /// The sidebar's "Needs you" filter is on (task 3.4).
    filter: bool,
}

impl Global for Notifier {}

impl Notifier {
    /// Whether the sidebar shows only the sessions that need the person.
    pub(crate) fn filter_on(cx: &App) -> bool {
        cx.try_global::<Notifier>().is_some_and(|n| n.filter)
    }

    /// The badge last set, for tests.
    #[cfg(test)]
    pub(crate) fn badge(cx: &App) -> Option<usize> {
        cx.try_global::<Notifier>().and_then(|n| n.badge)
    }
}

impl Harness {
    /// Look at every row's state and post what changed (see the module
    /// docs). Cheap when the list has not moved since the last look.
    pub(crate) fn check_attention(&mut self, cx: &mut Context<Self>) {
        let epoch = self.session_list.read(cx).epoch;
        let notifier = cx.default_global::<Notifier>();
        if notifier.epoch == Some(epoch) {
            return;
        }
        let first = notifier.epoch.is_none();
        let before = std::mem::take(&mut notifier.states);
        let front = self.active_id(cx);
        let prefs = crate::layout::Layout::get(cx).notify_prefs();
        let look = transitions(&before, self.session_list.read(cx).sessions.iter(), front.as_deref(), app_is_active(), prefs);
        let badge = if prefs.badge { look.needs_you } else { 0 };
        let notifier = cx.default_global::<Notifier>();
        notifier.states = look.states;
        notifier.epoch = Some(epoch);
        let badge_changed = notifier.badge != Some(badge);
        notifier.badge = Some(badge);
        if badge_changed {
            set_dock_badge(badge);
        }
        // The first look only learns (a launch announces nothing), and asks
        // macOS once for permission to notify — see [`ask_permission`].
        if first {
            if prefs.needs_you || prefs.done || prefs.failed {
                ask_permission();
            }
            return;
        }
        for post in look.posts {
            cx.show_system_notification(SystemNotification {
                tag: SharedString::from(post.session_id),
                title: SharedString::from(post.title),
                body: SharedString::from(post.body),
                actions: Vec::new(),
            });
            if prefs.sound {
                play_sound();
            }
        }
    }

    /// ⌘⇧J and `/next-waiting`: open the next session that needs the
    /// person, in the sidebar's order, wrapping; from Browse it switches to
    /// Sessions first. Nothing waiting does nothing.
    pub(crate) fn jump_to_next_waiting(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let visible = self.visible_sessions(cx);
        let order: Vec<String> = waiting(visible.iter()).into_iter().map(|row| row.id.clone()).collect();
        let current = self.active_id(cx);
        let Some(next) = next_waiting(&order, current.as_deref()) else { return };
        if crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse {
            self.set_mode(crate::layout::Mode::Sessions, window, cx);
        }
        self.resume_quiet(next, window, cx);
    }

    /// The sidebar's "Needs you" row: show only the waiting sessions, or
    /// all again.
    pub(crate) fn toggle_needs_you_filter(&mut self, cx: &mut Context<Self>) {
        let notifier = cx.default_global::<Notifier>();
        notifier.filter = !notifier.filter;
        self.invalidate_list(cx);
    }

    #[cfg(feature = "probe")]
    /// `needs-you-filter:<on|off>`: the filter set, not toggled, so a
    /// capture's second run changes nothing.
    pub(crate) fn step_needs_you_filter(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let want = match rest.trim() {
            "on" => true,
            "off" => false,
            other => return Err(format!("needs-you-filter takes on or off, not `{other}`")),
        };
        if Notifier::filter_on(cx) != want {
            self.toggle_needs_you_filter(cx);
        }
        Ok(())
    }

    #[cfg(feature = "probe")]
    /// `row-needs-you:<label>`: the sidebar row with that label starts
    /// waiting for an approval, as a wire status would make it, and the app
    /// takes its look. Offline: the live check's only way to make a real
    /// notification (and the permission prompt before the first) appear
    /// without a model turn. No such row, or one already waiting, is an
    /// `Err`.
    pub(crate) fn step_row_needs_you(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let label = rest.trim();
        self.session_list.update(cx, |list, _| {
            let row = list
                .sessions
                .iter_mut()
                .find(|row| row.label.eq_ignore_ascii_case(label))
                .ok_or_else(|| format!("no sidebar row is labelled `{label}`"))?;
            if matches!(row.session_state(), SessionState::NeedsYou(_)) {
                return Err(format!("`{label}` is already waiting"));
            }
            row.approval_command = Some("cargo test --workspace".to_owned());
            Ok(())
        })?;
        self.invalidate_list(cx);
        self.check_attention(cx);
        Ok(())
    }

    /// Wire a notification click to its session: bring Baaz forward and
    /// open the session the notification named.
    pub(crate) fn on_notification_click(cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        cx.on_system_notification_response(move |response, cx: &mut App| {
            let this = this.clone();
            let session_id = response.tag.to_string();
            cx.activate(true);
            cx.defer(move |cx| {
                let Some(this) = this.upgrade() else { return };
                let Some(window) = cx.windows().into_iter().next() else { return };
                let _ = window.update(cx, |_, window, cx| {
                    this.update(cx, |harness, cx| harness.resume_quiet(session_id, window, cx));
                });
            });
        });
    }
}

/// Whether Baaz is the frontmost app: a session "in front" is only being
/// looked at while it is.
fn app_is_active() -> bool {
    if cfg!(test) || crate::runtime::captures() {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        use objc2::{msg_send, runtime::AnyObject};
        // SAFETY: `sharedApplication` and `isActive` are plain AppKit
        // queries, called on the main thread like every UI call here.
        unsafe {
            let app: *mut AnyObject = msg_send![objc2::class!(NSApplication), sharedApplication];
            if app.is_null() {
                return false;
            }
            let active: bool = msg_send![app, isActive];
            active
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Set the Dock icon's badge to `count`, or clear it at zero. Tests and
/// deterministic captures never touch the Dock.
/// Ask macOS for permission to post notifications, at launch.
///
/// gpui asks only as it posts the first notification, and macOS drops a
/// notification posted before the person has answered: live on the check
/// build (17 Nov) the first "Waiting for your approval" never showed, only
/// the permission prompt did. Asking at the first look moves the prompt to
/// launch, so the first real alert lands. Once answered, macOS remembers and
/// this does nothing visible. The answer is logged.
///
/// Outside an app bundle `UNUserNotificationCenter` aborts the process
/// (gpui guards the same way), so a bare binary — every capture — skips it.
fn ask_permission() {
    if cfg!(test) || crate::runtime::captures() {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        use objc2::runtime::{AnyClass, AnyObject, Bool};
        use objc2::msg_send;
        // SAFETY: Foundation and UserNotifications calls from the main
        // thread; every object is checked for nil before use, and the
        // completion block only logs.
        unsafe {
            let bundle: *mut AnyObject = msg_send![objc2::class!(NSBundle), mainBundle];
            if bundle.is_null() {
                return;
            }
            let id: *mut AnyObject = msg_send![bundle, bundleIdentifier];
            if id.is_null() {
                return;
            }
            let Some(class) = AnyClass::get(c"UNUserNotificationCenter") else { return };
            let center: *mut AnyObject = msg_send![class, currentNotificationCenter];
            if center.is_null() {
                return;
            }
            let done = block2::RcBlock::new(|granted: Bool, error: *mut AnyObject| {
                crate::baaz_log!(
                    "notifications: permission {}{}",
                    if granted.as_bool() { "granted" } else { "not granted" },
                    if error.is_null() { "" } else { " (error)" }
                );
            });
            // UNAuthorizationOptionSound | UNAuthorizationOptionAlert, the
            // same options gpui asks for.
            let options: usize = (1 << 1) | (1 << 2);
            let _: () = msg_send![center, requestAuthorizationWithOptions: options, completionHandler: &*done];
        }
    }
}

fn set_dock_badge(count: usize) {
    if cfg!(test) || crate::runtime::captures() {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        use objc2::{msg_send, runtime::AnyObject};
        use objc2_foundation::NSString;
        // SAFETY: AppKit's dock tile, set from the main thread; a nil label
        // clears the badge.
        unsafe {
            let app: *mut AnyObject = msg_send![objc2::class!(NSApplication), sharedApplication];
            if app.is_null() {
                return;
            }
            let tile: *mut AnyObject = msg_send![app, dockTile];
            if tile.is_null() {
                return;
            }
            if count == 0 {
                let none: *const NSString = std::ptr::null();
                let _: () = msg_send![tile, setBadgeLabel: none];
            } else {
                let label = NSString::from_str(&count.to_string());
                let _: () = msg_send![tile, setBadgeLabel: &*label];
            }
        }
    }
}

/// The system's short notification sound, for Settings' sound switch.
fn play_sound() {
    if cfg!(test) || crate::runtime::captures() {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        use objc2::{msg_send, runtime::AnyObject};
        use objc2_foundation::NSString;
        // SAFETY: `NSSound soundNamed:` returns a shared autoreleased sound
        // or nil; `play` is safe on the main thread.
        unsafe {
            let name = NSString::from_str("Glass");
            let sound: *mut AnyObject = msg_send![objc2::class!(NSSound), soundNamed: &*name];
            if !sound.is_null() {
                let _: bool = msg_send![sound, play];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidebar::tests::entry;
    use provider::facts::Attention;

    fn states(pairs: &[(&str, SessionState)]) -> HashMap<String, SessionState> {
        pairs.iter().map(|(id, s)| ((*id).to_owned(), *s)).collect()
    }

    /// The first look learns every state and posts nothing; the badge
    /// still counts.
    #[test]
    fn a_first_sighting_posts_nothing() {
        let mut waiting = entry("a");
        waiting.attention = vec![Attention::ApprovalPending];
        let look = transitions(&HashMap::new(), [&waiting], None, false, NotifyPrefs::default());
        assert!(look.posts.is_empty());
        assert_eq!(look.needs_you, 1);
        assert_eq!(look.states.get("a"), Some(&SessionState::NeedsYou(NeedKind::Approval)));
    }

    /// Working → needs you, → done-unread and → failed each post once,
    /// with words that name no provider.
    #[test]
    fn the_three_moves_post() {
        let before = states(&[("q", SessionState::Working), ("d", SessionState::Working), ("f", SessionState::Working)]);
        let mut asked = entry("q");
        asked.label = "Fix the header".into();
        asked.pending_question = Some("Which file?".into());
        let mut done = entry("d");
        done.unread = true;
        let mut failed = entry("f");
        failed.last_error = Some("boom".into());
        let look = transitions(&before, [&asked, &done, &failed], None, false, NotifyPrefs::default());
        let bodies: Vec<&str> = look.posts.iter().map(|p| p.body.as_str()).collect();
        assert_eq!(bodies, ["Asks: Which file?", "Finished", "Stopped with an error"]);
        assert_eq!(look.posts[0].title, "Fix the header");
        assert_eq!(look.posts[0].session_id, "q");
        assert_eq!(look.needs_you, 1);
    }

    /// Staying in a state, idle → unread (a store load, not a finish) and
    /// the session in front of an active app post nothing; in front of an
    /// inactive app it posts.
    #[test]
    fn quiet_cases_stay_quiet() {
        let mut waiting = entry("w");
        waiting.attention = vec![Attention::InputPending];
        let still = states(&[("w", SessionState::NeedsYou(NeedKind::Question))]);
        assert!(transitions(&still, [&waiting], None, false, NotifyPrefs::default()).posts.is_empty());
        let mut unread = entry("u");
        unread.unread = true;
        let idle = states(&[("u", SessionState::Idle)]);
        assert!(transitions(&idle, [&unread], None, false, NotifyPrefs::default()).posts.is_empty());
        let working = states(&[("w", SessionState::Working)]);
        assert!(transitions(&working, [&waiting], Some("w"), true, NotifyPrefs::default()).posts.is_empty(), "the person is looking at it");
        assert_eq!(transitions(&working, [&waiting], Some("w"), false, NotifyPrefs::default()).posts.len(), 1, "the app is behind another one");
    }

    /// ⌘⇧J walks the waiting sessions in order and wraps; from a session
    /// that is not waiting it starts at the first; nothing waiting is None.
    #[test]
    fn next_waiting_cycles_in_order() {
        let order: Vec<String> = ["a", "b", "c"].iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(next_waiting(&order, None).as_deref(), Some("a"));
        assert_eq!(next_waiting(&order, Some("a")).as_deref(), Some("b"));
        assert_eq!(next_waiting(&order, Some("b")).as_deref(), Some("c"));
        assert_eq!(next_waiting(&order, Some("c")).as_deref(), Some("a"), "wraps");
        assert_eq!(next_waiting(&order, Some("x")).as_deref(), Some("a"), "from a session not waiting");
        assert_eq!(next_waiting(&[], Some("a")), None);
        let mut w1 = entry("w1");
        w1.attention = vec![Attention::ApprovalPending];
        let idle = entry("i");
        let mut w2 = entry("w2");
        w2.pending_question = Some("?".into());
        let ids: Vec<&str> = waiting([&w1, &idle, &w2]).into_iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["w1", "w2"], "in the rows' order, idle skipped");
    }

    /// Hidden, archived and side sessions are neither counted nor announced;
    /// a switched-off event is not announced but still counted.
    #[test]
    fn prefs_and_hidden_rows() {
        let working = states(&[("h", SessionState::Working), ("v", SessionState::Working)]);
        let mut hidden = entry("h");
        hidden.hidden = true;
        hidden.attention = vec![Attention::ApprovalPending];
        let mut visible = entry("v");
        visible.attention = vec![Attention::ApprovalPending];
        let quiet = NotifyPrefs { needs_you: false, ..NotifyPrefs::default() };
        let look = transitions(&working, [&hidden, &visible], None, false, quiet);
        assert!(look.posts.is_empty());
        assert_eq!(look.needs_you, 1, "only the visible row counts");
    }
}
