//! Favicons for the Browse sidebar (owner 2026-10-11: "favicons not
//! shown"). The engine reports each page's icon URL
//! ([`WebviewState::favicon`](aui_webview::WebviewState::favicon)); the tab
//! keeps it ([`super::tabs::Tab::favicon`]) so a discarded or relaunched tab
//! still has one, and this cache turns a URL into a local file the sidebar
//! paints with `gpui::img`.
//!
//! One file per icon URL under `<state>/favicons/`, named by a hash of the
//! URL: raster icons (ICO, PNG, GIF, JPEG, WebP) are decoded and stored as a
//! 32 px PNG (two device pixels per point at 16 pt); SVG is kept as SVG;
//! `data:` URLs decode in process. A fetch runs off the main thread through
//! `/usr/bin/curl` (no HTTP client lives in the app), once per URL per
//! launch; a failure leaves the letter tile.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The stored raster size, in pixels.
const ICON_PX: u32 = 32;

/// URL → local file, plus the URLs asked for this launch.
#[derive(Default)]
pub(crate) struct Favicons {
    files: HashMap<String, PathBuf>,
    asked: HashSet<String>,
    /// Bumped when a file lands, so the cached sidebar repaints.
    pub generation: u64,
}

impl Favicons {
    /// The local file for `url`, when one is ready.
    pub(crate) fn file(&self, url: &str) -> Option<&PathBuf> {
        self.files.get(url)
    }

    /// Whether `url` still needs a fetch: never asked this launch. A file
    /// already on disk from an earlier launch is adopted at once.
    pub(crate) fn needs_fetch(&mut self, url: &str) -> bool {
        if url.is_empty() || self.asked.contains(url) {
            return false;
        }
        self.asked.insert(url.to_owned());
        if let Some(path) = existing(url) {
            self.files.insert(url.to_owned(), path);
            self.generation += 1;
            return false;
        }
        true
    }

    /// Record a finished fetch.
    pub(crate) fn landed(&mut self, url: String, path: Option<PathBuf>) {
        if let Some(path) = path {
            self.files.insert(url, path);
            self.generation += 1;
        }
    }
}

fn dir() -> PathBuf {
    crate::store::support_dir().join("favicons")
}

/// The file name stem for `url`.
fn stem(url: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// A stored icon for `url` from an earlier launch.
fn existing(url: &str) -> Option<PathBuf> {
    let stem = stem(url);
    ["png", "svg"].iter().map(|ext| dir().join(format!("{stem}.{ext}"))).find(|path| path.exists())
}

/// Fetch and store the icon at `url`. Blocking: call off the main thread.
pub(crate) fn fetch(url: &str) -> Option<PathBuf> {
    let bytes = if let Some(rest) = url.strip_prefix("data:") {
        decode_data_url(rest)?
    } else if url.starts_with("http://") || url.starts_with("https://") {
        let out = std::process::Command::new("/usr/bin/curl")
            .args(["-sfL", "--max-time", "8", "--max-filesize", "1048576", "-A", "Mozilla/5.0 (Macintosh) Baaz", url])
            .output()
            .ok()?;
        if !out.status.success() || out.stdout.is_empty() {
            return None;
        }
        out.stdout
    } else {
        return None;
    };
    let dir = dir();
    std::fs::create_dir_all(&dir).ok()?;
    store(&dir, &stem(url), &bytes)
}

/// Write `bytes` as `<stem>.svg` when it is SVG text, else decode and
/// write a [`ICON_PX`] PNG.
fn store(dir: &Path, stem: &str, bytes: &[u8]) -> Option<PathBuf> {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(256)]).to_ascii_lowercase();
    if head.contains("<svg") || (head.trim_start().starts_with("<?xml") && String::from_utf8_lossy(bytes).contains("<svg")) {
        let path = dir.join(format!("{stem}.svg"));
        std::fs::write(&path, bytes).ok()?;
        return Some(path);
    }
    let image = image::load_from_memory(bytes).ok()?;
    let icon = image.resize(ICON_PX, ICON_PX, image::imageops::FilterType::Lanczos3);
    let path = dir.join(format!("{stem}.png"));
    icon.save(&path).ok()?;
    Some(path)
}

/// The payload of a `data:` URL (after the scheme): base64 or
/// percent-encoded text.
fn decode_data_url(rest: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let (meta, payload) = rest.split_once(',')?;
    if meta.ends_with(";base64") {
        base64::engine::general_purpose::STANDARD.decode(payload.trim()).ok()
    } else {
        Some(percent_decode(payload))
    }
}

fn percent_decode(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&text[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_urls_decode_both_ways() {
        assert_eq!(decode_data_url("image/svg+xml,%3Csvg%3E").unwrap(), b"<svg>");
        assert_eq!(decode_data_url("image/png;base64,aGk=").unwrap(), b"hi");
    }

    #[test]
    fn svg_is_kept_and_raster_is_shrunk_to_32() {
        let dir = std::env::temp_dir().join(format!("baaz-favicon-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let svg = store(&dir, "a", b"<svg xmlns='http://www.w3.org/2000/svg'/>").unwrap();
        assert_eq!(svg.extension().unwrap(), "svg");
        let mut png = Vec::new();
        image::DynamicImage::new_rgba8(64, 64).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let raster = store(&dir, "b", &png).unwrap();
        assert_eq!(image::open(&raster).unwrap().width(), ICON_PX);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_url_is_asked_once_per_launch() {
        let mut cache = Favicons::default();
        let url = "https://never-fetched.invalid/favicon.ico";
        assert!(cache.needs_fetch(url));
        assert!(!cache.needs_fetch(url));
        assert!(!cache.needs_fetch(""));
    }
}
