//! The new tab's ground (owner 2026-10-11, canvas v27 "B with C's
//! background"): the Mac's own desktop picture, blurred and dimmed, with
//! option B's centre scrim so the Field and the pick-up list keep their
//! contrast. Nothing in Fluid Functionalism draws a picture ground; this is
//! the owner's departure from FF's flat greys, and only the ground departs —
//! the Field and rows on it stay FF.
//!
//! gpui has neither an image blur nor a radial gradient, so the ground is
//! baked once off the main thread into a small PNG under the state dir
//! (`newtab/ground-<key>.png`, keyed by the source path, size and mtime) and
//! the page paints that file with `object-fit: cover`. Blur, dim and scrim
//! are all baked in.
//!
//! The source, in order:
//! 1. `NSWorkspace.desktopImageURLForScreen:` for the main screen (HEIC and
//!    other formats go through `/usr/bin/sips` first) — only when the
//!    picture lives outside `~/Library` (the system's pictures, the person's
//!    own files): under `~/Library` it is another app's data, and reading it
//!    raised macOS's "Baaz would like to access data from other apps" on a
//!    blank tab the person never asked about (owner 2026-11-28). The aerial
//!    still that used to come first (`com.apple.wallpaper/aerials/…`, read
//!    from Apple's wallpaper store) is gone for the same reason; an aerial
//!    wallpaper is a video, and `desktopImageURLForScreen:` then answers the
//!    stock picture, so an aerial now shows the stock one;
//! 2. none readable: option B's mesh (indigo, teal, plum), generated.

use std::path::{Path, PathBuf};

use image::{Rgb, RgbImage};

/// The baked image's longest side, in pixels: the blur makes detail moot,
/// and a small file decodes in a frame.
const GROUND_MAX: u32 = 320;
/// The blur, as a standard deviation in baked pixels: CSS `blur(60px)` over
/// a ~1170 pt card painted from 320 px (≈ 3.65 pt per pixel).
const GROUND_SIGMA: f32 = 16.0;
/// The dim over the picture: black at 40 % (canvas board C).
const GROUND_DIM: f32 = 0.40;
/// Option B's centre scrim: rgb(15,15,18) at 45 % in the middle of an
/// ellipse 45 % × 50 % of the card at (50 %, 38 %), fading out by 75 %.
const SCRIM: [f32; 3] = [15.0, 15.0, 18.0];
const SCRIM_ALPHA: f32 = 0.45;

/// The baked ground for `source` (from [`current_source`], read on the
/// main thread — `NSScreen` is main-thread only): the cached file when it
/// exists, else built now. Blocking (decode, blur, encode ≈ tens of ms):
/// call it off the main thread.
pub(crate) fn bake(source: Option<PathBuf>) -> Option<PathBuf> {
    let dir = crate::store::support_dir().join("newtab");
    let (source, key) = match source {
        Some(source) => {
            let key = source_key(&source)?;
            (Some(source), key)
        }
        None => (None, "mesh-v1".to_owned()),
    };
    let out = dir.join(format!("ground-{key}.png"));
    if out.exists() {
        return Some(out);
    }
    let picture = match source.as_deref().and_then(load_small) {
        Some(picture) => {
            let mut blurred = image::imageops::blur(&picture, GROUND_SIGMA);
            dim(&mut blurred, GROUND_DIM);
            blurred
        }
        None => mesh(),
    };
    let mut baked = picture;
    scrim(&mut baked);
    std::fs::create_dir_all(&dir).ok()?;
    // Old grounds go: one wallpaper's bake is all the page shows.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("ground-") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    baked.save(&out).ok()?;
    Some(out)
}

/// What the desktop shows now, as a readable still: the desktop picture,
/// when it is one Baaz may read without asking (see the module doc).
pub(crate) fn current_source() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    desktop_picture().filter(|path| readable_without_asking(path, home.as_deref())).filter(|path| path.exists())
}

/// Whether reading `path` stays off other apps' data: nothing under the
/// person's `~/Library` (containers, group containers, Application Support
/// — where macOS asks before one app reads another's files).
fn readable_without_asking(path: &Path, home: Option<&Path>) -> bool {
    match home {
        Some(home) => !path.starts_with(home.join("Library")),
        None => true,
    }
}

/// `NSWorkspace.desktopImageURLForScreen:` for the main screen.
#[cfg(target_os = "macos")]
fn desktop_picture() -> Option<PathBuf> {
    use objc2::{class, msg_send, runtime::AnyObject};
    // SAFETY: plain AppKit class-method and getter calls; every object is
    // checked for nil before use and nothing is retained past this scope.
    unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        if workspace.is_null() || screen.is_null() {
            return None;
        }
        let url: *mut AnyObject = msg_send![workspace, desktopImageURLForScreen: screen];
        if url.is_null() {
            return None;
        }
        let path: *mut AnyObject = msg_send![url, path];
        if path.is_null() {
            return None;
        }
        let utf8: *const std::ffi::c_char = msg_send![path, UTF8String];
        if utf8.is_null() {
            return None;
        }
        Some(PathBuf::from(std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned()))
    }
}

#[cfg(not(target_os = "macos"))]
fn desktop_picture() -> Option<PathBuf> {
    None
}

/// The cache key: the source's path, size and mtime, hashed.
fn source_key(path: &Path) -> Option<String> {
    use std::hash::{Hash, Hasher};
    let meta = std::fs::metadata(path).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    meta.len().hash(&mut hasher);
    meta.modified().ok().hash(&mut hasher);
    Some(format!("{:016x}", hasher.finish()))
}

/// The picture, decoded and scaled to [`GROUND_MAX`]: PNG and JPEG in
/// process, anything else (HEIC) through `sips` into a temp PNG.
fn load_small(path: &Path) -> Option<RgbImage> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let decoded = if matches!(ext.as_str(), "png" | "jpg" | "jpeg") {
        image::open(path).ok()?
    } else {
        let tmp = std::env::temp_dir().join(format!("baaz-ground-{}.png", std::process::id()));
        let status = std::process::Command::new("/usr/bin/sips")
            .args(["-s", "format", "png", "-Z", &GROUND_MAX.to_string()])
            .arg(path)
            .arg("--out")
            .arg(&tmp)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok()?;
        let decoded = if status.success() { image::open(&tmp).ok() } else { None };
        let _ = std::fs::remove_file(&tmp);
        decoded?
    };
    Some(decoded.resize(GROUND_MAX, GROUND_MAX, image::imageops::FilterType::Triangle).to_rgb8())
}

/// Black over the whole picture at `alpha`.
fn dim(picture: &mut RgbImage, alpha: f32) {
    for px in picture.pixels_mut() {
        for c in px.0.iter_mut() {
            *c = (f32::from(*c) * (1.0 - alpha)).round() as u8;
        }
    }
}

/// Option B's centre scrim, baked: the scrim colour over each pixel at
/// [`SCRIM_ALPHA`] in the ellipse's middle, falling linearly to nothing at
/// 75 % of its radius (CSS `radial-gradient(45% 50% at 50% 38%, …, 0 75%)`).
fn scrim(picture: &mut RgbImage) {
    let (w, h) = picture.dimensions();
    let (cx, cy) = (w as f32 * 0.5, h as f32 * 0.38);
    let (rx, ry) = (w as f32 * 0.45, h as f32 * 0.50);
    for (x, y, px) in picture.enumerate_pixels_mut() {
        let dx = (x as f32 + 0.5 - cx) / rx;
        let dy = (y as f32 + 0.5 - cy) / ry;
        let t = ((dx * dx + dy * dy).sqrt() / 0.75).min(1.0);
        let alpha = SCRIM_ALPHA * (1.0 - t);
        for (c, s) in px.0.iter_mut().zip(SCRIM) {
            *c = (f32::from(*c) * (1.0 - alpha) + s * alpha).round() as u8;
        }
    }
}

/// Option B's mesh (canvas board B), for a desktop with no readable
/// picture: four colour glows over #15151C, each fading out by 70 % of its
/// ellipse, then blurred lightly so the blend has no seams.
fn mesh() -> RgbImage {
    let (w, h) = (GROUND_MAX, GROUND_MAX * 5 / 8);
    // (centre x, centre y, radius x, radius y) as fractions, then colour.
    let glows: [([f32; 4], [f32; 3]); 4] = [
        ([0.12, 0.10, 0.55, 0.60], [58.0, 46.0, 122.0]),
        ([0.92, 0.18, 0.50, 0.55], [15.0, 92.0, 110.0]),
        ([0.70, 1.00, 0.65, 0.60], [110.0, 44.0, 85.0]),
        ([0.30, 0.85, 0.45, 0.45], [29.0, 62.0, 122.0]),
    ];
    let mut picture = RgbImage::from_pixel(w, h, Rgb([21, 21, 28]));
    for (x, y, px) in picture.enumerate_pixels_mut() {
        let mut rgb = [f32::from(px.0[0]), f32::from(px.0[1]), f32::from(px.0[2])];
        // Painted back to front, like stacked CSS backgrounds.
        for (geo, colour) in glows.iter().rev() {
            let dx = (x as f32 / w as f32 - geo[0]) / geo[2];
            let dy = (y as f32 / h as f32 - geo[1]) / geo[3];
            let alpha = (1.0 - (dx * dx + dy * dy).sqrt() / 0.70).clamp(0.0, 1.0);
            for (c, s) in rgb.iter_mut().zip(colour) {
                *c = *c * (1.0 - alpha) + s * alpha;
            }
        }
        px.0 = [rgb[0].round() as u8, rgb[1].round() as u8, rgb[2].round() as u8];
    }
    image::imageops::blur(&picture, 2.0)
}

#[cfg(feature = "probe")]
#[cfg(test)]
mod tests {
    use super::*;

    /// Owner 2026-11-28: a blank tab raised macOS's "access data from
    /// other apps". The ground reads no picture under ~/Library; the
    /// system's pictures and the person's own files stay readable.
    #[test]
    fn the_ground_never_reads_another_apps_data() {
        let home = Path::new("/Users/ada");
        for other in [
            "/Users/ada/Library/Application Support/com.apple.wallpaper/aerials/thumbnails/x.png",
            "/Users/ada/Library/Containers/com.apple.wallpaper.agent/Data/x.heic",
            "/Users/ada/Library/Group Containers/group.com.apple.photos/x.jpg",
        ] {
            assert!(!readable_without_asking(Path::new(other), Some(home)), "{other}");
        }
        for fine in ["/System/Library/Desktop Pictures/Sequoia.heic", "/Users/ada/Pictures/beach.jpg", "/Library/Desktop Pictures/x.jpg"] {
            assert!(readable_without_asking(Path::new(fine), Some(home)), "{fine}");
        }
        let source = include_str!("newtab_ground.rs");
        assert!(!source.contains(&["com.apple.", "wallpaper/Store"].concat()), "the wallpaper store is never read");
    }

    #[test]
    fn the_scrim_darkens_the_middle_and_spares_the_edges() {
        let mut picture = RgbImage::from_pixel(100, 100, Rgb([200, 200, 200]));
        scrim(&mut picture);
        let middle = picture.get_pixel(50, 38).0[0];
        let corner = picture.get_pixel(0, 99).0[0];
        assert!(middle < 130, "middle {middle}");
        assert_eq!(corner, 200);
    }

    #[test]
    fn the_mesh_is_the_fallback_size_and_not_flat() {
        let picture = mesh();
        assert_eq!(picture.dimensions(), (GROUND_MAX, GROUND_MAX * 5 / 8));
        assert_ne!(picture.get_pixel(10, 10), picture.get_pixel(GROUND_MAX - 10, 10));
    }
}
