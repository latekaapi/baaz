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
//! 1. the current aerial's still (`com.apple.wallpaper/aerials/thumbnails/
//!    <assetID>.png`) — an aerial wallpaper is a video, and
//!    `desktopImageURLForScreen:` then answers the stock `DefaultDesktop.heic`
//!    instead of what is on screen (seen 2026-10-11);
//! 2. `NSWorkspace.desktopImageURLForScreen:` for the main screen (HEIC and
//!    other formats go through `/usr/bin/sips` first);
//! 3. none readable: option B's mesh (indigo, teal, plum), generated.

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

/// What the desktop shows now, as a readable still: the aerial's still
/// when an aerial is set, else the desktop picture.
pub(crate) fn current_source() -> Option<PathBuf> {
    aerial_still().or_else(desktop_picture).filter(|path| path.exists())
}

/// The current aerial's thumbnail. The wallpaper store's `Index.plist` is a
/// binary plist whose aerial choices nest their configuration as raw binary
/// plist bytes, so the asset id appears in the file as plain ASCII: the
/// first UUID-shaped run that names an existing thumbnail is the one set.
fn aerial_still() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let root = Path::new(&home).join("Library/Application Support/com.apple.wallpaper");
    let index = std::fs::read(root.join("Store/Index.plist")).ok()?;
    let thumbs = root.join("aerials/thumbnails");
    uuids(&index).into_iter().map(|id| thumbs.join(format!("{id}.png"))).find(|path| path.exists())
}

/// Every 8-4-4-4-12 hex run in `bytes`, in order.
fn uuids(bytes: &[u8]) -> Vec<String> {
    const SHAPE: [usize; 5] = [8, 4, 4, 4, 12];
    let mut found = Vec::new();
    let mut i = 0;
    while i + 36 <= bytes.len() {
        let candidate = &bytes[i..i + 36];
        let mut at = 0;
        let mut ok = true;
        for (n, len) in SHAPE.iter().enumerate() {
            if !candidate[at..at + len].iter().all(u8::is_ascii_hexdigit) {
                ok = false;
                break;
            }
            at += len;
            if n < 4 {
                if candidate[at] != b'-' {
                    ok = false;
                    break;
                }
                at += 1;
            }
        }
        if ok {
            found.push(String::from_utf8_lossy(candidate).into_owned());
            i += 36;
        } else {
            i += 1;
        }
    }
    found
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuids_are_found_inside_binary_noise() {
        let mut bytes = vec![0u8, 0x5f, 0x10, 0x24];
        bytes.extend_from_slice(b"52ACB9B8-75FC-4516-BC60-4550CFF3B661");
        bytes.extend_from_slice(&[8, 11, 0x13, b'z']);
        bytes.extend_from_slice(b"not-a-uuid-1234");
        assert_eq!(uuids(&bytes), vec!["52ACB9B8-75FC-4516-BC60-4550CFF3B661".to_owned()]);
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
