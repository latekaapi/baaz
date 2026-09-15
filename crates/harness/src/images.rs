//! Images in the composer: paste, drop and attach.
//!
//! MSP's `TurnInputPart` for an image is `{type:"image", base64Data, mediaType,
//! width?, height?}`, with width and height **together or not at all**
//! (`msp.d.ts:1443–1456`). The Phase 3 probe confirmed the echo provider accepts
//! such a part (`status: accepted`), so nothing here needed a real turn to
//! prove — and it accepted bytes that are not a decodable PNG, which says the
//! wire checks the base64 and the media type and never looks at the image.
//!
//! Everything is decoded here, once, when the image enters the composer: the
//! bytes are validated as a real image, the media type comes from the bytes
//! rather than the file name, and the dimensions come from the decoder. A file
//! that is not one of the four formats, or is over [`MAX_BYTES`], is refused
//! with a reason the banner can show.

use std::path::Path;

use base64::Engine as _;

/// The per-image cap. Ten megabytes of base64 is already a 13 MB JSON line.
pub const MAX_BYTES: usize = 10 * 1024 * 1024;

/// The thumbnail's long edge: a 64 px preview for the chip, decoded once at
/// attach time, while the full-resolution bytes still go on the wire.
pub const THUMB_LONG_EDGE: u32 = 64;

/// One image ready to be sent, and to be drawn as a composer chip.
#[derive(Clone)]
pub struct Image {
    /// Stable id for the chip and its remove button.
    pub id: String,
    /// What the chip says: a file name, or `pasted.png`.
    pub name: String,
    /// `image/png`, `image/jpeg`, `image/gif`, `image/webp`.
    pub media_type: String,
    /// The payload, already base64.
    pub base64_data: String,
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// The downscaled preview the chip draws; dropped on remove/send.
    pub thumb: Option<std::sync::Arc<gpui::RenderImage>>,
    /// The read, the decode and the thumbnail have not finished yet.
    ///
    /// A ten-megabyte photo used to be read and decoded inline in the drop,
    /// paste or step handler, stalling the frame it landed on (finding
    /// `performance-14`). The chip goes up straight away as a placeholder —
    /// the name, no preview — and [`placeholder`] is what makes it; the work
    /// runs on the background executor and replaces this entry when it lands.
    /// Nothing may be sent while one of these is in the composer: it has no
    /// bytes yet, so [`Image::part`] would send an empty payload.
    pub pending: bool,
}

/// The chip that stands in while an image is read and decoded.
pub fn placeholder(id: impl Into<String>, name: impl Into<String>) -> Image {
    Image {
        id: id.into(),
        name: name.into(),
        media_type: String::new(),
        base64_data: String::new(),
        width: 0,
        height: 0,
        thumb: None,
        pending: true,
    }
}

/// What a file name says the chip should be called before anything is read.
pub fn display_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "image".to_owned())
}

impl PartialEq for Image {
    fn eq(&self, other: &Self) -> bool {
        // The thumbnail is a render cache, not content: two decodes of the
        // same bytes compare equal.
        self.id == other.id
            && self.name == other.name
            && self.media_type == other.media_type
            && self.base64_data == other.base64_data
            && self.width == other.width
            && self.height == other.height
            && self.pending == other.pending
    }
}

impl Eq for Image {}

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `RenderImage` has no `Debug`, so the thumbnail reports presence only.
        f.debug_struct("Image")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("media_type", &self.media_type)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("thumbnail", &self.thumb.is_some())
            .field("pending", &self.pending)
            .finish_non_exhaustive()
    }
}

/// The four formats MSP's image part is worth sending.
fn media_type(format: image::ImageFormat) -> Option<&'static str> {
    match format {
        image::ImageFormat::Png => Some("image/png"),
        image::ImageFormat::Jpeg => Some("image/jpeg"),
        image::ImageFormat::Gif => Some("image/gif"),
        image::ImageFormat::WebP => Some("image/webp"),
        _ => None,
    }
}

/// Turn raw bytes into an [`Image`], or say why not.
///
/// The format is guessed from the bytes: a `.png` that is really a JPEG would
/// otherwise be sent with the wrong media type, and the provider would be the
/// one to find out.
pub fn from_bytes(id: impl Into<String>, name: impl Into<String>, bytes: &[u8]) -> Result<Image, String> {
    if bytes.is_empty() {
        return Err("the image is empty".to_owned());
    }
    if bytes.len() > MAX_BYTES {
        // One decimal, like `attachments::human_bytes` already does (finding
        // `support-9`): integer division rounded a 10.9 MB image down to
        // "10 MB".
        return Err(format!(
            "the image is {:.1} MB; the limit is {:.1} MB",
            bytes.len() as f64 / 1_048_576.0,
            MAX_BYTES as f64 / 1_048_576.0
        ));
    }
    let format = image::guess_format(bytes).map_err(|_| "that is not an image".to_owned())?;
    let media_type = media_type(format).ok_or_else(|| format!("{format:?} images are not supported"))?;
    let decoded = image::load_from_memory_with_format(bytes, format)
        .map_err(|error| format!("the image could not be read: {error}"))?;
    let (width, height) = (decoded.width(), decoded.height());
    // The chip's preview, downscaled once here so no frame ever decodes.
    let preview = decoded.thumbnail(THUMB_LONG_EDGE, THUMB_LONG_EDGE).into_rgba8();
    let thumb = std::sync::Arc::new(gpui::RenderImage::new(vec![image::Frame::new(preview)]));
    let mut name: String = name.into();
    if name.is_empty() {
        name = format!("pasted.{}", format.extensions_str().first().copied().unwrap_or("png"));
    }
    Ok(Image {
        id: id.into(),
        name,
        media_type: media_type.to_owned(),
        base64_data: base64::engine::general_purpose::STANDARD.encode(bytes),
        width,
        height,
        thumb: Some(thumb),
        pending: false,
    })
}

/// Read a file from disk and decode it.
pub fn from_path(id: impl Into<String>, path: &Path) -> Result<Image, String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    from_bytes(id, name, &bytes)
}

impl Image {
    /// The wire part. Width and height go together or not at all, and they
    /// always come from the decoder here, so they always go together.
    pub fn part(&self) -> muse_client::schema::TurnInputPart {
        muse_client::schema::TurnInputPart {
            r#type: muse_client::schema::TurnInputPartType::Image,
            arguments: None,
            base64_data: Some(self.base64_data.clone()),
            media_type: Some(self.media_type.clone()),
            selector: None,
            width: Some(self.width),
            height: Some(self.height),
            text: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real 1×1 transparent PNG, CRCs and all.
    ///
    /// The bytes `fixtures/msp/probe_phase3.py` (removed 2026-09-12; git
    /// history has it) sent were **not** a valid PNG — its IDAT CRC was wrong
    /// — and MSP accepted the part anyway, which was the probe's other
    /// finding: the wire validates base64 and the media type and never
    /// decodes the image. The app does, here, so these bytes are the real
    /// thing.
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
        0x89, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x60, 0x00, 0x02, 0x00,
        0x00, 0x05, 0x00, 0x01, 0x7a, 0x5e, 0xab, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44,
        0xae, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn a_png_decodes_to_a_wire_part() {
        let image = from_bytes("i1", "shot.png", PNG).expect("decodes");
        assert_eq!(image.media_type, "image/png");
        assert_eq!((image.width, image.height), (1, 1));
        let part = image.part();
        assert!(part.base64_data.is_some() && part.media_type.is_some());
        // Width and height must be present together or not at all.
        assert_eq!(part.width.is_some(), part.height.is_some());
    }

    #[test]
    fn a_pasted_image_gets_a_name() {
        let image = from_bytes("i1", "", PNG).expect("decodes");
        assert!(image.name.starts_with("pasted."));
    }

    #[test]
    fn something_that_is_not_an_image_is_refused() {
        assert!(from_bytes("i1", "notes.txt", b"hello there").is_err());
    }

    #[test]
    fn an_empty_payload_is_refused() {
        assert!(from_bytes("i1", "x.png", b"").is_err());
    }
}
