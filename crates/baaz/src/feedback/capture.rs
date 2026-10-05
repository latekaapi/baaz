//! Capture helpers: pure functions over `image::RgbaImage` plus a thin
//! wrapper over the window's own render.
//!
//! Everything here stays at device pixels for crispness except
//! [`to_logical`], which downsamples like `--screenshot` does.

use super::Rect;
use image::RgbaImage;

/// The outline drawn around a note's target, `#E8A64B`.
const OUTLINE: image::Rgba<u8> = image::Rgba([0xE8, 0xA6, 0x4B, 255]);
/// The outline's thickness, in device pixels.
const OUTLINE_PX: i64 = 2;

/// Crop the target grown by `pad` logical pixels, with a 2-device-pixel
/// `#E8A64B` outline around the target itself.
///
/// The crop is clamped to the image; a target at the edge yields a smaller
/// crop rather than a panic. Away from the edges the crop is exactly
/// `(target + 2·pad) × scale` device pixels. The image keeps device pixels.
pub fn crop_with_outline(full: &RgbaImage, scale: f32, target: Rect, pad: f32) -> RgbaImage {
    let scale = sanitize_scale(scale);
    let (img_w, img_h) = (full.width(), full.height());
    let img_w_logical = img_w as f32 / scale;
    let img_h_logical = img_h as f32 / scale;
    // The grown crop in logical pixels, clamped to the image.
    let x0_logical = (target.x - pad).clamp(0.0, img_w_logical);
    let y0_logical = (target.y - pad).clamp(0.0, img_h_logical);
    let x1_logical = (target.x + target.w + pad).clamp(0.0, img_w_logical);
    let y1_logical = (target.y + target.h + pad).clamp(0.0, img_h_logical);
    // Round the origin and the extent separately so an unclamped crop is
    // exactly `(target + 2·pad) × scale`, independent of position.
    let mut x0 = (x0_logical * scale).round().clamp(0.0, img_w as f32) as u32;
    let mut y0 = (y0_logical * scale).round().clamp(0.0, img_h as f32) as u32;
    let mut width = ((x1_logical - x0_logical) * scale).round().clamp(0.0, img_w as f32) as u32;
    let mut height = ((y1_logical - y0_logical) * scale).round().clamp(0.0, img_h as f32) as u32;
    width = width.max(1).min(img_w.saturating_sub(x0).max(1));
    height = height.max(1).min(img_h.saturating_sub(y0).max(1));
    if x0 >= img_w {
        x0 = img_w - 1;
    }
    if y0 >= img_h {
        y0 = img_h - 1;
    }
    let mut crop = image::imageops::crop_imm(full, x0, y0, width, height).to_image();
    // The target in crop-relative device pixels; parts outside the crop
    // (when the target itself hangs off the image) are skipped pixel by pixel.
    let target_x = (target.x * scale).round() as i64 - x0 as i64;
    let target_y = (target.y * scale).round() as i64 - y0 as i64;
    let target_w = (target.w * scale).round() as i64;
    let target_h = (target.h * scale).round() as i64;
    for dy in 0..target_h {
        for dx in 0..target_w {
            let on_edge = dx < OUTLINE_PX
                || dy < OUTLINE_PX
                || dx >= target_w - OUTLINE_PX
                || dy >= target_h - OUTLINE_PX;
            if !on_edge {
                continue;
            }
            let px = target_x + dx;
            let py = target_y + dy;
            if px < 0 || py < 0 || px >= width as i64 || py >= height as i64 {
                continue;
            }
            crop.put_pixel(px as u32, py as u32, OUTLINE);
        }
    }
    crop
}

/// Downsample a device-pixel shot to logical pixels with Lanczos3, the same
/// filter `--screenshot` uses.
pub fn to_logical(full: &RgbaImage, scale: f32) -> RgbaImage {
    let scale = sanitize_scale(scale);
    let (width, height) = (full.width(), full.height());
    let target_w = ((width as f32 / scale).round() as u32).max(1);
    let target_h = ((height as f32 / scale).round() as u32).max(1);
    if (target_w, target_h) == (width, height) {
        return full.clone();
    }
    image::imageops::resize(full, target_w, target_h, image::imageops::FilterType::Lanczos3)
}

/// Render the window at device pixels. Callers own refresh/frame timing:
/// refresh first and render on a later frame, as `--screenshot` does.
pub fn render_window(window: &mut gpui::Window) -> anyhow::Result<RgbaImage> {
    window.render_to_image()
}

/// A non-positive or non-finite scale would divide by zero or empty the
/// crop; treat it as 1×.
fn sanitize_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    /// A solid device-pixel image.
    fn solid(width: u32, height: u32) -> RgbaImage {
        RgbaImage::from_pixel(width, height, Rgba([10, 20, 30, 255]))
    }

    #[test]
    fn unclamped_crop_has_the_grown_size_and_an_outline() {
        let full = solid(80, 60);
        let target = Rect { x: 10.0, y: 8.0, w: 12.0, h: 10.0 };
        let crop = crop_with_outline(&full, 2.0, target, 3.0);
        assert_eq!((crop.width(), crop.height()), (36, 32));
        // The target's top-left corner inside the crop wears the outline.
        let corner_x = (10.0_f32 * 2.0).round() as u32 - (7.0_f32 * 2.0).round() as u32;
        let corner_y = (8.0_f32 * 2.0).round() as u32 - (5.0_f32 * 2.0).round() as u32;
        assert_eq!(crop.get_pixel(corner_x, corner_y), &OUTLINE);
        // So does at least one edge pixel; the middle does not.
        assert!(crop.pixels().any(|pixel| *pixel == OUTLINE));
        let middle = crop.get_pixel(corner_x + 6, corner_y + 6);
        assert_eq!(middle, &Rgba([10, 20, 30, 255]));
    }

    #[test]
    fn a_target_off_the_edge_clamps_without_panic() {
        let full = solid(20, 20);
        // Logical 10×10 at 2×; the grown box hangs well off the corner.
        let target = Rect { x: 8.0, y: 8.0, w: 4.0, h: 4.0 };
        let crop = crop_with_outline(&full, 2.0, target, 3.0);
        assert_eq!((crop.width(), crop.height()), (10, 10));
    }

    #[test]
    fn downsampling_halves_a_retina_shot() {
        let full = solid(40, 30);
        let logical = to_logical(&full, 2.0);
        assert_eq!((logical.width(), logical.height()), (20, 15));
        let same = to_logical(&full, 1.0);
        assert_eq!((same.width(), same.height()), (40, 30));
    }
}
