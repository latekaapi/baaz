//! `--screenshot`: render the window off-screen once it has settled,
//! downsample to logical pixels and quit.
//!
//! Lifted from the gallery's own capture path (`aui-gallery/src/shot.rs`) so a
//! harness PNG and a gallery PNG are the same kind of image: 1×, no window
//! chrome, no pointer.

use std::{path::PathBuf, time::Duration};

use gpui::{App, WindowHandle};
use gpui_kit::component::Root;

/// Waits for the first frames, captures the window and exits the process.
pub fn capture_and_quit(handle: WindowHandle<Root>, path: PathBuf, delay: Duration, cx: &mut App) {
    cx.spawn(async move |cx| {
        cx.background_executor().timer(delay).await;
        let result = cx.update(|cx| {
            handle.update(cx, |_root, window, _cx| {
                let scale = window.scale_factor();
                let image = window.render_to_image()?;
                let (w, h) = (image.width(), image.height());
                let target_w = (w as f32 / scale).round() as u32;
                let target_h = (h as f32 / scale).round() as u32;
                let image = if (target_w, target_h) != (w, h) {
                    image::imageops::resize(&image, target_w, target_h, image::imageops::FilterType::Lanczos3)
                } else {
                    image
                };
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                image.save(&path)?;
                anyhow::Ok((target_w, target_h))
            })
        });
        match result {
            Ok(Ok((w, h))) => println!("wrote {} ({w}\u{d7}{h})", path.display()),
            Ok(Err(err)) => eprintln!("screenshot failed: {err:#}"),
            Err(err) => eprintln!("screenshot failed: {err:#}"),
        }
        cx.update(|cx| cx.quit());
    })
    .detach();
}
