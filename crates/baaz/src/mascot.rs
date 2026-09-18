//! The Baaz mascot: bundled PNGs served through a harness asset source.
//!
//! `aui::assets::AuiAssets` serves the library's own compiled-in art
//! (`aui-icons` layered over gpui-kit's set). It cannot see files that live
//! in this crate, so the mascot PNGs live under
//! `crates/baaz/assets/mascot/` and are embedded here with `include_bytes!`.
//! [`BaazAssets`] serves the `mascot/…` paths and chains to [`AuiAssets`]
//! for everything else; the app installs it in `main.rs` (both the shell
//! boot and the bench boot), so `gpui::img("mascot/perch/…")` resolves
//! through the normal embedded-asset pipeline.
//!
//! Retina note: gpui's image loader fetches exactly the embedded path it is
//! given — it never auto-requests `@2x`/`@3x` siblings — so every mascot
//! path (base, `@2x`, `@3x`) serves the `@3x` bytes. The element lays out at
//! the design size (44/56/180 pt) and the GPU downscales, which stays crisp
//! on retina. The 1x/2x files are still copied into the repo as the
//! resolution ladder's record.

use std::borrow::Cow;
use std::time::Duration;

use aui_motion::{EnterExit, PresenceStyle, presence};
use gpui::{AssetSource, SharedString, div, img, prelude::*, px, AnyElement, App, Window};

/// The five `perch/` variants (44 pt), in asset order.
pub const PERCH_PATHS: [&str; 5] = [
    "mascot/perch/welcome-idle.png",
    "mascot/perch/greeting-wave.png",
    "mascot/perch/chill-ambient.png",
    "mascot/perch/thinking-planning.png",
    "mascot/perch/searching-reading.png",
];

/// The boot-screen mascot (180 pt).
pub const BOOT_GREETING: &str = "mascot/boot/greeting-wave.png";

/// Layout size of the perch mascot, in points.
pub const PERCH_PT: f32 = 44.0;
/// Layout size of the boot mascot, in points.
pub const BOOT_PT: f32 = 180.0;
/// Layout size of the error mascot, in points.
pub const ERROR_PT: f32 = 56.0;

/// The `error/` variants (56 pt), one per error family.
pub const ERROR_SORRY: &str = "mascot/error/error-sorry.png";
/// Network, connectivity and provider-unreachable failures.
pub const ERROR_OFFLINE: &str = "mascot/error/offline-disconnected.png";
/// Usage-limit, permission and auth refusals.
pub const ERROR_BLOCKED: &str = "mascot/error/blocked-permission.png";

/// One mascot file: its base asset path and its `@3x` bytes.
struct MascotFile {
    base: &'static str,
    bytes_3x: &'static [u8],
}

macro_rules! mascot {
    ($dir:literal, $name:literal) => {
        MascotFile {
            base: concat!("mascot/", $dir, "/", $name, ".png"),
            bytes_3x: include_bytes!(concat!("../assets/mascot/", $dir, "/", $name, "@3x.png")),
        }
    };
}

/// Every bundled mascot file. The loader normalises any `@2x`/`@3x` request
/// to the base path, so all three suffixes serve these bytes.
static MASCOTS: &[MascotFile] = &[
    mascot!("perch", "welcome-idle"),
    mascot!("perch", "greeting-wave"),
    mascot!("perch", "chill-ambient"),
    mascot!("perch", "thinking-planning"),
    mascot!("perch", "searching-reading"),
    mascot!("boot", "greeting-wave"),
    mascot!("boot", "welcome-idle"),
    mascot!("error", "error-sorry"),
    mascot!("error", "offline-disconnected"),
    mascot!("error", "blocked-permission"),
];

/// The `@3x` bytes for a mascot path, whatever its suffix.
fn mascot_bytes(path: &str) -> Option<&'static [u8]> {
    // Compare against the stem: `mascot/perch/foo[@2x|@3x].png` all match
    // `mascot/perch/foo.png`. Anything outside `mascot/` is not ours.
    let (stem, _) = path.rsplit_once('.')?;
    let stem = stem.strip_suffix("@3x").or_else(|| stem.strip_suffix("@2x")).unwrap_or(stem);
    MASCOTS.iter().find(|m| m.base.strip_suffix(".png") == Some(stem)).map(|m| m.bytes_3x as &'static [u8])
}

/// Which perch variant a session gets: FNV-1a over the session id, mod five.
///
/// Deterministic per session id, so the variant stays stable while the
/// new-session screen is open and never reshuffles frame to frame; different
/// sessions land on different variants.
pub fn perch_index_for_session(session_id: &str) -> usize {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in session_id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    (hash % PERCH_PATHS.len() as u64) as usize
}

/// Which error family a critical dialog is in. Picked by error kind — the
/// title and detail a failure carries — never at random.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorMascot {
    /// Everything else critical.
    Sorry,
    /// Network, connectivity and provider-unreachable failures.
    Offline,
    /// Usage-limit, permission and auth refusals.
    Blocked,
}

impl ErrorMascot {
    /// The asset path for this family.
    pub fn path(self) -> &'static str {
        match self {
            ErrorMascot::Sorry => ERROR_SORRY,
            ErrorMascot::Offline => ERROR_OFFLINE,
            ErrorMascot::Blocked => ERROR_BLOCKED,
        }
    }

    /// Classify a critical dialog's title and detail into a family.
    ///
    /// Substring matching over the lowercased pair, transport first: a
    /// refusal only counts as [`ErrorMascot::Blocked`] with an entitlement
    /// signal beside it (limit, quota, permission, auth), so a bare "Muse
    /// refused the command" stays [`ErrorMascot::Sorry`].
    pub fn classify(title: &str, detail: &str) -> Self {
        let hay = format!("{title}\n{detail}").to_lowercase();
        const OFFLINE: &[&str] = &[
            "disconnect",
            "did not answer",
            "could not be started",
            "not ready yet",
            "timeout",
            "timed out",
            "connection",
            "connectivity",
            "network",
            "offline",
            "unreachable",
            "socket",
            "econn",
            "eai_again",
            "broken pipe",
        ];
        if OFFLINE.iter().any(|needle| hay.contains(needle)) {
            return ErrorMascot::Offline;
        }
        const BLOCKED: &[&str] = &[
            "usage limit",
            "limit exceeded",
            "quota",
            "rate limit",
            "rate-limit",
            "permission",
            "denied",
            "forbidden",
            "unauthor",
            "not authenticated",
            "not signed in",
            "no credential",
            "signed out",
            "auth refus",
            "auth fail",
            "auth error",
            "blocked",
        ];
        if BLOCKED.iter().any(|needle| hay.contains(needle)) {
            return ErrorMascot::Blocked;
        }
        ErrorMascot::Sorry
    }
}

/// The critical-dialog mascot: 56 pt on the dialog's leading side, drawn
/// beside the title by its caller.
///
/// In-flow and fixed-size, so its space is reserved and nothing shifts when
/// the PNG lands. Static — no enter, no hover, no glow, halo or coloured
/// rail — so a deterministic capture matches a live open, in light and dark.
pub fn error_mascot(kind: ErrorMascot) -> AnyElement {
    div().flex_none().child(img(kind.path()).w(px(ERROR_PT)).h(px(ERROR_PT))).into_any_element()
}

/// The harness asset source: mascot paths from [`MASCOTS`], everything else
/// from [`AuiAssets`].
#[derive(Debug, Clone, Copy, Default)]
pub struct BaazAssets;

impl AssetSource for BaazAssets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = mascot_bytes(path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        aui::assets::AuiAssets.load(path)
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        let mut out = aui::assets::AuiAssets.list(path)?;
        match path {
            "" => out.push("mascot".into()),
            "mascot" => out.extend(["mascot/perch".into(), "mascot/boot".into(), "mascot/error".into()]),
            "mascot/perch" | "mascot/boot" | "mascot/error" => {
                let dir = path;
                out.extend(
                    MASCOTS
                        .iter()
                        .filter(|m| m.base.starts_with(dir))
                        .map(|m| SharedString::from(m.base.to_owned())),
                );
            }
            _ => {}
        }
        Ok(out)
    }
}

/// The boot-screen mascot: 180 pt, centred by its caller, fading in with a
/// slight upward drift (8 px, ~320 ms, ease-out), once on appear.
///
/// In-flow and fixed-size, so its space is reserved and nothing shifts when
/// it appears. Under reduced motion it draws settled.
pub fn boot_mascot(window: &mut Window, cx: &mut App) -> AnyElement {
    let style = if cx.reduce_motion() {
        PresenceStyle { opacity: 1.0, offset_y: px(0.0), scale: 1.0 }
    } else {
        let timing = EnterExit {
            enter: Duration::from_millis(320),
            exit: Duration::from_millis(160),
            delay: Duration::ZERO,
        };
        // One stable id: the enter plays once, on appear — never again on
        // re-render, because the presence is already `Present`.
        PresenceStyle::fade_rise(presence("baaz-boot-mascot", true, timing, window, cx), 8.0)
    };
    div()
        .flex()
        .items_center()
        .justify_center()
        .opacity(style.opacity)
        .relative()
        .top(style.offset_y)
        .child(img(BOOT_GREETING).w(px(BOOT_PT)).h(px(BOOT_PT)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_path_resolves_whatever_its_suffix() {
        for file in MASCOTS {
            let base = mascot_bytes(file.base).expect("base path resolves");
            assert!(!base.is_empty());
            let stem = file.base.strip_suffix(".png").unwrap();
            for suffixed in [format!("{stem}@2x.png"), format!("{stem}@3x.png")] {
                assert_eq!(mascot_bytes(&suffixed), Some(base), "{suffixed} serves the same bytes");
            }
        }
        assert!(mascot_bytes("icons/foo.svg").is_none(), "non-mascot paths are not ours");
    }

    #[test]
    fn the_seed_is_stable_per_session_and_varies_across_sessions() {
        assert_eq!(perch_index_for_session("s-1"), perch_index_for_session("s-1"));
        for id in ["s-1", "s-2", "new-session", "01234567-89ab-cdef-0123-456789abcdef"] {
            assert!(perch_index_for_session(id) < PERCH_PATHS.len(), "{id} is in range");
        }
        let distinct: std::collections::HashSet<usize> =
            ["s-1", "s-2", "s-3", "s-4", "s-5", "s-6", "s-7", "s-8"]
                .iter()
                .map(|id| perch_index_for_session(id))
                .collect();
        assert!(distinct.len() >= 2, "eight sessions share one variant: {distinct:?}");
    }

    #[test]
    fn the_error_families_classify_by_kind_never_at_random() {
        // Transport first: the offline family.
        for (title, detail) in [
            ("Muse disconnected", "the child exited"),
            ("Muse did not answer turn/start", "timed out after 180s"),
            ("Muse could not be started", "spawn failed: no such file"),
            ("Muse is not ready yet", "initialize is still in flight"),
            ("Something went wrong", "connection reset by peer"),
        ] {
            assert_eq!(ErrorMascot::classify(title, detail), ErrorMascot::Offline, "{title}");
        }
        // Entitlement signals: the blocked family.
        for (title, detail) in [
            ("Usage limit exceeded", "quota resets at midnight"),
            ("Muse refused the turn", "meta is not authenticated"),
            ("Signed out of Muse", "Muse refused the turn: not signed in"),
            ("Something went wrong", "permission denied for key"),
        ] {
            assert_eq!(ErrorMascot::classify(title, detail), ErrorMascot::Blocked, "{title}");
        }
        // A bare refusal names no entitlement, so it stays sorry.
        for (title, detail) in [
            ("Muse hit an internal error", "internal: index out of bounds"),
            ("Session already in use", "s1 is held by another window"),
            ("Muse refused the command", "commandRejected: bad state"),
            ("Sign out failed", "account/logout answered -32603"),
            ("Something went wrong", "no further detail"),
        ] {
            assert_eq!(ErrorMascot::classify(title, detail), ErrorMascot::Sorry, "{title}");
        }
    }

    #[test]
    fn every_error_sprite_resolves_and_lists() {
        for sprite in [ErrorMascot::Sorry, ErrorMascot::Offline, ErrorMascot::Blocked] {
            assert!(!mascot_bytes(sprite.path()).unwrap_or_default().is_empty(), "{sprite:?} resolves");
        }
        let error = BaazAssets.list("mascot/error").expect("error lists");
        assert_eq!(error.len(), 3, "three error sprites: {error:?}");
        assert_eq!(ErrorMascot::Sorry.path(), ERROR_SORRY);
        assert_eq!(ErrorMascot::Offline.path(), ERROR_OFFLINE);
        assert_eq!(ErrorMascot::Blocked.path(), ERROR_BLOCKED);
        assert_eq!(ERROR_PT, 56.0);
    }

    #[test]
    fn the_source_lists_the_mascot_tree_beside_the_library() {
        let root = BaazAssets.list("").expect("root lists");
        assert!(root.iter().any(|e| e.as_ref() == "mascot"), "mascot is listed: {root:?}");
        let perch = BaazAssets.list("mascot/perch").expect("perch lists");
        assert_eq!(perch.len(), PERCH_PATHS.len());
        for path in PERCH_PATHS {
            assert!(perch.iter().any(|e| e.as_ref() == path), "{path} is listed");
        }
    }
}
