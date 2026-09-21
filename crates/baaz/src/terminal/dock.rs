//! The dock's own numbers: its height, and the clamp it lives under.
//!
//! The dock hangs under the composer in the centre column (D42). Its open
//! state and height persist in [`Layout`][crate::layout::Layout]; the height
//! is clamped to `[120px, 70% of the centre column]`.

/// The dock's shortest height, in window pixels.
pub const DOCK_MIN_HEIGHT: f32 = 120.0;

/// The dock's height before the person ever resized it.
pub const DOCK_DEFAULT_HEIGHT: f32 = 260.0;

/// Clamp a dock height into `[120px, 70% of the centre column]`.
pub fn clamp_dock_height(want: f32, centre_height: f32) -> f32 {
    let max = (centre_height * 0.7).max(DOCK_MIN_HEIGHT);
    want.clamp(DOCK_MIN_HEIGHT, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dock_never_leaves_its_range() {
        assert_eq!(clamp_dock_height(260.0, 800.0), 260.0);
        assert_eq!(clamp_dock_height(10.0, 800.0), DOCK_MIN_HEIGHT);
        assert_eq!(clamp_dock_height(10_000.0, 800.0), 560.0);
        assert_eq!(clamp_dock_height(10_000.0, 100.0), DOCK_MIN_HEIGHT);
    }
}
