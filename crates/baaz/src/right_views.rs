//! D6's pushed views in Baaz that any screen can carry (docs/43 §2.9–2.10):
//! the full output of a command and an image. The transcript opens them on
//! the screen the pane shows (with the pane closed, on its last screen); ←,
//! Esc and ⌘[ pop them; ✕ closes the pane.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use aui::workbench::{image_facts, image_missing, image_view, output_fetching, output_title, output_view, pane_action, pushed_row, pushed_row_with};
use aui_icons::IconName;
use gpui::{div, prelude::*, px, AnyElement, ClipboardItem, Context, IntoElement, SharedString, Window};

use crate::app::Harness;

/// Where a full output's lines come from.
#[derive(Clone, Debug)]
pub(crate) enum OutputLines {
    /// Still being fetched from the provider.
    Fetching,
    /// The lines, and the total when capped.
    Ready { lines: Arc<Vec<String>>, capped: Option<usize> },
}

/// A full-output view.
#[derive(Clone, Debug)]
pub(crate) struct OutputPush {
    /// The command as it ran.
    pub command: String,
    pub exit: Option<i32>,
    /// The run's time, as shown (`43.8 s`).
    pub time: Option<String>,
    /// Still running since: the header tails and counts.
    pub running_since: Option<Instant>,
    pub lines: OutputLines,
    /// For a fetched output: the session and block it belongs to, so a
    /// finished fetch lands on the right view.
    pub source: Option<(String, String)>,
}

/// An image view.
#[derive(Clone, Debug)]
pub(crate) struct ImagePush {
    pub name: String,
    pub path: PathBuf,
    /// Natural size, kind and bytes, when readable.
    pub facts: Option<(u32, u32, String, u64)>,
    /// 100 % instead of fitted.
    pub actual: bool,
}

/// A view pushed on a screen.
#[derive(Clone, Debug)]
pub(crate) enum Pushed {
    Output(OutputPush),
    Image(ImagePush),
}

/// The natural size of an image file, from its header (PNG, JPEG, GIF, WebP).
pub(crate) fn image_size(path: &std::path::Path) -> Option<(u32, u32)> {
    use std::io::Read as _;
    let mut head = vec![0u8; 64 * 1024];
    let n = std::fs::File::open(path).ok()?.read(&mut head).ok()?;
    imagesize_from(&head[..n])
}

/// Width × height from an image's first bytes.
fn imagesize_from(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() > 24 && &b[..8] == b"\x89PNG\r\n\x1a\n" {
        return Some((u32::from_be_bytes(b[16..20].try_into().ok()?), u32::from_be_bytes(b[20..24].try_into().ok()?)));
    }
    if b.len() > 10 && &b[..6] == b"GIF89a" || b.len() > 10 && &b[..6] == b"GIF87a" {
        return Some((u16::from_le_bytes([b[6], b[7]]) as u32, u16::from_le_bytes([b[8], b[9]]) as u32));
    }
    if b.len() > 4 && b[0] == 0xFF && b[1] == 0xD8 {
        let mut i = 2;
        while i + 9 < b.len() {
            if b[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = b[i + 1];
            let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
            if (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC {
                let h = u16::from_be_bytes([b[i + 5], b[i + 6]]) as u32;
                let w = u16::from_be_bytes([b[i + 7], b[i + 8]]) as u32;
                return Some((w, h));
            }
            i += 2 + len;
        }
    }
    if b.len() > 30 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" && &b[12..16] == b"VP8X" {
        let w = 1 + (b[24] as u32 | (b[25] as u32) << 8 | (b[26] as u32) << 16);
        let h = 1 + (b[27] as u32 | (b[28] as u32) << 8 | (b[29] as u32) << 16);
        return Some((w, h));
    }
    None
}

impl Harness {
    /// Push `view` on the screen the pane shows (opening the pane on its
    /// last screen when closed).
    pub(crate) fn push_right_view(&mut self, view: Pushed, cx: &mut Context<Self>) {
        let kind = crate::layout::right_kind(&self.layout);
        if !self.layout.right_open {
            self.select_right(kind, cx);
        }
        let screen = crate::right::screen_index(kind);
        self.right_cache.pushed.insert(screen, view);
        self.right_cache.pushed_at.insert(screen, Instant::now());
        cx.notify();
    }

    /// Pop the view pushed on `screen`; true when one was there.
    pub(crate) fn pop_right_view(&mut self, screen: usize, cx: &mut Context<Self>) -> bool {
        let popped = self.right_cache.pushed.remove(&screen).is_some();
        if popped {
            cx.notify();
        }
        popped
    }

    /// A shell card's full output (docs/43 §2.9): the card's lines at once,
    /// or "Fetching…" while the provider pages the rest in.
    pub(crate) fn open_tool_output(&mut self, out: crate::transcript::OutputOpen, cx: &mut Context<Self>) {
        let session = self.active.as_ref().map(|view| view.read(cx).session_id.clone()).unwrap_or_default();
        let lines = if out.fetchable { OutputLines::Fetching } else { OutputLines::Ready { lines: Arc::new(out.lines.clone()), capped: None } };
        self.push_right_view(
            Pushed::Output(OutputPush {
                command: out.command.clone(),
                exit: out.exit,
                time: out.duration_ms.map(|ms| format!("{:.1} s", ms as f64 / 1000.0)),
                running_since: out.live.then(Instant::now),
                lines,
                source: Some((session, out.block_id.clone())),
            }),
            cx,
        );
    }

    /// Keep pushed output views current: a fetch that landed, a command
    /// still printing. Reads the active session view; called before render.
    pub(crate) fn sync_pushed_outputs(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.active.clone() else { return };
        let session = view.read(cx).session_id.clone();
        for pushed in self.right_cache.pushed.values_mut() {
            let Pushed::Output(out) = pushed else { continue };
            let Some((sid, block)) = out.source.clone() else { continue };
            if sid != session {
                continue;
            }
            let Some((lines, live, exit, fetched)) = view.read(cx).tool_output(&block) else { continue };
            match (&out.lines, fetched) {
                (OutputLines::Fetching, Some((full, capped))) => {
                    let total = full.len();
                    out.lines = OutputLines::Ready { lines: Arc::new(full), capped: capped.then_some(total) };
                }
                (OutputLines::Ready { lines: shown, .. }, None) if out.running_since.is_some() && shown.len() != lines.len() => {
                    out.lines = OutputLines::Ready { lines: Arc::new(lines), capped: None };
                }
                _ => {}
            }
            if !live && out.running_since.is_some() {
                out.running_since = None;
                out.exit = exit;
            }
        }
    }

    /// The pre-commit hook's output as a full-output view on Changes.
    pub(crate) fn show_hook_output(&mut self, _root: &std::path::Path, lines: Vec<String>, cx: &mut Context<Self>) {
        self.push_right_view(
            Pushed::Output(OutputPush {
                command: "git commit".into(),
                exit: Some(1),
                time: None,
                running_since: None,
                lines: OutputLines::Ready { lines: Arc::new(lines), capped: None },
                source: None,
            }),
            cx,
        );
    }

    /// An image file or attachment in the image view.
    pub(crate) fn show_image(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let facts = std::fs::metadata(&path).ok().and_then(|meta| {
            let (w, h) = image_size(&path)?;
            let kind = path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
            Some((w, h, kind, meta.len()))
        });
        self.push_right_view(Pushed::Image(ImagePush { name, path, facts, actual: false }), cx);
    }
}

/// The pushed view's row and body for `screen`, when one is pushed.
pub(crate) fn pushed_screen(screen: usize, view: &Pushed, width: f32, window: &mut Window, cx: &mut Context<Harness>) -> (AnyElement, AnyElement) {
    let back = cx.listener(move |this: &mut Harness, _: &gpui::ClickEvent, _, cx| {
        this.pop_right_view(screen, cx);
    });
    let close = cx.listener(|this: &mut Harness, _: &gpui::ClickEvent, _, cx| this.toggle_right(cx));
    match view {
        Pushed::Output(out) => {
            let running = out.running_since.map(|t| t.elapsed().as_secs());
            if running.is_some() {
                aui_motion::request_frame(window);
            }
            let title = output_title(&out.command, out.exit, out.time.clone().map(Into::into), running, cx);
            let ready = matches!(out.lines, OutputLines::Ready { .. });
            let text = match &out.lines {
                OutputLines::Ready { lines, .. } => lines.join("\n"),
                OutputLines::Fetching => String::new(),
            };
            let copy = pane_action(SharedString::from(format!("right-output-copy-{screen}")), IconName::Copy, "Copy the output")
                .disabled(!ready)
                .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(text.clone())));
            let row = pushed_row_with(SharedString::from(format!("right-output-row-{screen}")), title)
                .back_label("Back")
                .width(px(width))
                .action(copy)
                .on_back(back)
                .on_close(close)
                .into_any_element();
            let body = match &out.lines {
                OutputLines::Fetching => output_fetching(cx).into_any_element(),
                OutputLines::Ready { lines, capped } => output_view(SharedString::from(format!("right-output-{screen}")), lines.clone())
                    .capped(*capped)
                    .follow(out.running_since.is_some())
                    .into_any_element(),
            };
            (row, body)
        }
        Pushed::Image(image) => {
            let open_path = image.path.clone();
            let row = pushed_row(SharedString::from(format!("right-image-row-{screen}")), image.name.clone())
                .back_label("Back")
                .width(px(width))
                .action(pane_action(SharedString::from(format!("right-image-open-{screen}")), IconName::External, "Open in default app").on_click(move |_, _, cx| {
                    if open_path.exists() {
                        cx.open_with_system(&open_path);
                    }
                }))
                .on_back(back)
                .on_close(close)
                .into_any_element();
            let body = match image.facts.clone() {
                Some((w, h, kind, bytes)) => {
                    let facts = image_facts(w, h, &kind, bytes);
                    let toggle = cx.listener(move |this: &mut Harness, _: &gpui::ClickEvent, _, cx| {
                        if let Some(Pushed::Image(i)) = this.right_cache.pushed.get_mut(&screen) {
                            i.actual = !i.actual;
                        }
                        cx.notify();
                    });
                    image_view(SharedString::from(format!("right-image-{screen}")), image.path.clone(), facts, Some((w as f32, h as f32)), image.actual, window, cx)
                        .on_click(toggle)
                        .into_any_element()
                }
                None => image_missing(SharedString::from(format!("right-image-missing-{screen}")), &image.name, cx).into_any_element(),
            };
            (row, div().size_full().child(body).into_any_element())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_and_gif_headers_give_their_size() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&1440u32.to_be_bytes());
        png.extend_from_slice(&900u32.to_be_bytes());
        png.extend_from_slice(&[0; 8]);
        assert_eq!(imagesize_from(&png), Some((1440, 900)));
        let gif = b"GIF89a\x10\x00\x20\x00\0\0\0".to_vec();
        assert_eq!(imagesize_from(&gif), Some((16, 32)));
        assert_eq!(imagesize_from(b"not an image at all, no"), None);
    }
}
