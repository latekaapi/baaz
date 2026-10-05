//! Report writer: persist a [`Draft`](super::Draft) as a report folder for
//! Claude and render its deterministic Markdown and JSON.
//!
//! A report is `<root>/<YYYY-MM-DD-HHMMSS>/` holding `report.md`,
//! `report.json` and `shots/note-<n>-crop.png` plus
//! `shots/note-<n>-full.png` per note, with `<root>/latest` pointing at the
//! newest one. Only `latest` is written atomically; the report files land by
//! plain write inside their fresh directory.

use super::{human_role, Draft, Target};
use chrono::{DateTime, Local, SecondsFormat};
use std::path::{Path, PathBuf};

/// `support_dir()/feedback`: where reports live.
pub fn feedback_root() -> PathBuf {
    crate::store::support_dir().join("feedback")
}

/// Write the draft as a new timestamped report directory under `root`.
///
/// The directory is named for `ended` as `<YYYY-MM-DD-HHMMSS>`, gaining a
/// `-2`, `-3`, … suffix while it collides. Full shots are stored at logical
/// pixels via [`to_logical`](super::to_logical); crops land as held. Then
/// `<root>/latest` is rewritten atomically with the absolute report path.
/// Returns the report directory.
pub fn write_report(
    root: &Path,
    draft: &Draft,
    ended: DateTime<Local>,
    build: &str,
) -> anyhow::Result<PathBuf> {
    let stamp = ended.format("%Y-%m-%d-%H%M%S").to_string();
    std::fs::create_dir_all(root)?;
    // `create_dir` (not `create_dir_all`) claims the name: two writers on
    // the same second can never share a folder.
    let mut dir = root.join(&stamp);
    let mut suffix = 2;
    loop {
        match std::fs::create_dir(&dir) {
            Ok(()) => break,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                dir = root.join(format!("{stamp}-{suffix}"));
                suffix += 1;
            }
            Err(err) => return Err(err.into()),
        }
    }
    std::fs::create_dir_all(dir.join("shots"))?;
    // A shot the note never captured is not named: the report must not
    // point at files that do not exist.
    let shots: Vec<(String, String)> = draft
        .notes
        .iter()
        .map(|note| {
            let (crop, full) = standard_shots(note.index);
            (
                if note.crop.is_some() { crop } else { String::new() },
                if note.full.is_some() { full } else { String::new() },
            )
        })
        .collect();
    std::fs::write(dir.join("report.md"), render_markdown(draft, ended, build, &shots))?;
    let json = render_json(draft, ended, build);
    std::fs::write(dir.join("report.json"), serde_json::to_string_pretty(&json)?)?;
    for (note, (crop_name, full_name)) in draft.notes.iter().zip(shots.iter()) {
        if let (Some(crop), false) = (&note.crop, crop_name.is_empty()) {
            crop.save(dir.join(crop_name))?;
        }
        if let (Some(full), false) = (&note.full, full_name.is_empty()) {
            super::to_logical(full, note.context.scale).save(dir.join(full_name))?;
        }
    }
    let absolute =
        if dir.is_absolute() { dir.clone() } else { std::env::current_dir()?.join(&dir) };
    crate::store::write_atomic(&root.join("latest"), format!("{}\n", absolute.display()).as_bytes())?;
    Ok(dir)
}

/// One line of inline Markdown from untrusted text (accessibility labels,
/// titles): newlines fold to spaces and backticks become `'`, so a label can
/// neither split a heading nor close a code span early.
fn inline(text: &str) -> String {
    let flat = text.replace("\r\n", " ").replace(['\n', '\r'], " ").replace('`', "'");
    flat.trim().to_owned()
}

/// [`inline`], with an empty result named rather than left blank.
fn named(text: &str) -> String {
    let flat = inline(text);
    if flat.is_empty() { "(unnamed)".to_owned() } else { flat }
}

/// A Markdown table cell: [`inline`] plus an escaped `|`.
fn cell(text: &str) -> String {
    inline(text).replace('|', "\\|")
}

/// The crop/full shot paths for a 1-based note index.
fn standard_shots(index: usize) -> (String, String) {
    (format!("shots/note-{index}-crop.png"), format!("shots/note-{index}-full.png"))
}

/// Render the report Markdown deterministically, ending with exactly one
/// `\n`. `shot_names` runs parallel to `draft.notes` and names each note's
/// crop and full shot; a missing entry falls back to the standard name.
pub fn render_markdown(
    draft: &Draft,
    ended: DateTime<Local>,
    build: &str,
    shot_names: &[(String, String)],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Baaz feedback — {}\n", draft.started.format("%Y-%m-%d %H:%M")));
    out.push('\n');
    out.push_str("> How to use this report: each note names what the owner pointed at, the screen it was on, and their words. Open the crop first; the full shot shows surroundings. `Code id` is a gpui element id you can grep for.\n");
    out.push('\n');
    out.push_str("| Field | Value |\n|---|---|\n");
    out.push_str(&format!("| Build | {} |\n", cell(build)));
    let window = match draft.notes.first() {
        Some(note) => format!(
            "{} × {} @{}x · {}",
            round_int(note.context.window_w),
            round_int(note.context.window_h),
            format_scale(note.context.scale),
            note.context.theme
        ),
        None => "unknown".to_owned(),
    };
    out.push_str(&format!("| Window | {window} |\n"));
    out.push_str(&format!("| Started | {} |\n", iso(&draft.started)));
    out.push_str(&format!("| Ended | {} |\n", iso(&ended)));
    out.push_str(&format!("| Notes | {} |\n", draft.notes.len()));
    if draft.notes.is_empty() {
        out.push_str("\nNo notes.\n");
        return out;
    }
    for (position, note) in draft.notes.iter().enumerate() {
        let (title, suffix) = match &note.target {
            Target::Element(hit) => (named(&hit.label), inline(&pane_or_mode(note))),
            Target::Area { region, .. } => (
                "Area".to_owned(),
                inline(&region.clone().unwrap_or_else(|| pane_or_mode(note))),
            ),
        };
        out.push('\n');
        if suffix.is_empty() {
            out.push_str(&format!("## Note {} · {title}\n", note.index));
        } else {
            out.push_str(&format!("## Note {} · {title} · {suffix}\n", note.index));
        }
        out.push_str(&format!("Screen: {}\n", inline(&screen_line(note))));
        if let Target::Element(hit) = &note.target {
            let mut element = format!("`{}` ({})", named(&hit.label), human_role(&hit.role));
            if !hit.ancestors.is_empty() {
                let chain = hit
                    .ancestors
                    .iter()
                    .map(|(_, label)| format!("`{}`", named(label)))
                    .collect::<Vec<_>>()
                    .join(" › ");
                element.push_str(&format!(" in {chain}"));
            }
            out.push_str(&format!("Element: {element}\n"));
            if let Some(id) = &hit.element_id {
                match &hit.view {
                    Some(view) => out.push_str(&format!("Code id: `{}` ({})\n", inline(id), inline(view))),
                    None => out.push_str(&format!("Code id: `{}`\n", inline(id))),
                }
            }
        }
        let rect = match &note.target {
            Target::Element(hit) => &hit.rect,
            Target::Area { rect, .. } => rect,
        };
        out.push_str(&format!(
            "Rect: `x={} y={} {} × {}`\n",
            round_int(rect.x),
            round_int(rect.y),
            round_int(rect.w),
            round_int(rect.h)
        ));
        out.push_str("Note:\n");
        for line in note.text.split('\n') {
            out.push_str(&format!("> {line}\n"));
        }
        let (crop, full) = shot_names
            .get(position)
            .cloned()
            .unwrap_or_else(|| standard_shots(note.index));
        let named_shots: Vec<String> =
            [crop, full].into_iter().filter(|name| !name.is_empty()).map(|name| format!("`{name}`")).collect();
        if named_shots.is_empty() {
            out.push_str("Shots: none\n");
        } else {
            out.push_str(&format!("Shots: {}\n", named_shots.join(" · ")));
        }
    }
    out
}

/// Render the report JSON: `"schema": "baaz-feedback/1"` plus the same data
/// the Markdown carries, with standard shot paths.
pub fn render_json(draft: &Draft, ended: DateTime<Local>, build: &str) -> serde_json::Value {
    serde_json::json!({
        "schema": "baaz-feedback/1",
        "build": build,
        "started": iso(&draft.started),
        "ended": iso(&ended),
        "notes": draft.notes.iter().map(|note| {
            let (target, rect) = match &note.target {
                Target::Element(hit) => (
                    serde_json::json!({
                        "kind": "element",
                        "label": hit.label,
                        "role": hit.role,
                        "human_role": human_role(&hit.role),
                        "ancestors": hit.ancestors.iter().map(|(role, label)| {
                            serde_json::json!({"role": role, "label": label})
                        }).collect::<Vec<_>>(),
                        "element_id": hit.element_id,
                        "view": hit.view,
                    }),
                    &hit.rect,
                ),
                Target::Area { rect, region } => (
                    serde_json::json!({"kind": "area", "region": region}),
                    rect,
                ),
            };
            let context = &note.context;
            let (crop, full) = standard_shots(note.index);
            let crop = note.crop.as_ref().map(|_| crop);
            let full = note.full.as_ref().map(|_| full);
            serde_json::json!({
                "index": note.index,
                "target": target,
                "rect": {"x": rect.x, "y": rect.y, "w": rect.w, "h": rect.h},
                "text": note.text,
                "context": {
                    "mode": context.mode,
                    "project": context.project,
                    "session_id": context.session_id,
                    "session_title": context.session_title,
                    "right_pane": context.right_pane,
                    "page_url": context.page_url,
                    "settings_section": context.settings_section,
                    "window_w": context.window_w,
                    "window_h": context.window_h,
                    "scale": context.scale,
                    "theme": context.theme,
                },
                "shots": {"crop": crop, "full": full},
            })
        }).collect::<Vec<_>>(),
    })
}

/// The heading's trailing scope: the area's region, else the right pane,
/// else the mode.
fn pane_or_mode(note: &super::Note) -> String {
    if let Target::Area { region: Some(region), .. } = &note.target {
        return region.clone();
    }
    note.context.right_pane.clone().unwrap_or_else(|| note.context.mode.clone())
}

/// The `Screen:` line, omitting absent parts.
fn screen_line(note: &super::Note) -> String {
    let context = &note.context;
    let mut parts = Vec::new();
    if !context.mode.is_empty() {
        parts.push(context.mode.clone());
    }
    if let Some(project) = &context.project {
        parts.push(format!("project {project}"));
    }
    match (&context.session_title, &context.session_id) {
        (Some(title), Some(id)) => parts.push(format!("session \"{title}\" ({id})")),
        (Some(title), None) => parts.push(format!("session \"{title}\"")),
        (None, Some(id)) => parts.push(format!("session {id}")),
        (None, None) => {}
    }
    match (&context.right_pane, &context.page_url) {
        (Some(pane), Some(url)) => parts.push(format!("right pane {pane} ({url})")),
        (Some(pane), None) => parts.push(format!("right pane {pane}")),
        (None, _) => {}
    }
    if let Some(section) = &context.settings_section {
        parts.push(format!("Settings → {section}"));
    }
    parts.join(" · ")
}

/// A timestamp as `YYYY-MM-DDTHH:MM:SS±HH:MM`.
fn iso(at: &DateTime<Local>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, false)
}

/// A pixel value rounded to an integer for display.
fn round_int(value: f32) -> i64 {
    value.round() as i64
}

/// A scale factor for display: `2`, not `2.0`.
fn format_scale(scale: f32) -> String {
    let rounded = scale.round();
    if (scale - rounded).abs() < 1e-6 {
        format!("{rounded}")
    } else {
        format!("{scale}")
    }
}

#[cfg(test)]
mod tests {
    use super::super::{test_context, ElementHit, Rect, ScreenContext, Target};
    use super::*;
    use chrono::TimeZone;

    /// A two-note draft: one element note with ancestors and a code id, one
    /// area note; the first note's text spans two lines.
    fn golden_draft() -> (Draft, DateTime<Local>) {
        let started = Local.with_ymd_and_hms(2026, 9, 30, 9, 5, 7).unwrap();
        let ended = Local.with_ymd_and_hms(2026, 9, 30, 9, 41, 22).unwrap();
        let mut context = test_context();
        context.project = Some("baaz".to_owned());
        context.session_id = Some("abc".to_owned());
        context.session_title = Some("Fix the crash".to_owned());
        context.right_pane = Some("browser".to_owned());
        context.page_url = Some("https://example.com".to_owned());
        let mut draft = Draft { started, notes: Vec::new() };
        draft.add(
            Target::Element(ElementHit {
                label: "New session".to_owned(),
                role: "AXButton".to_owned(),
                ancestors: vec![
                    ("AXGroup".to_owned(), "Sidebar".to_owned()),
                    ("AXTabGroup".to_owned(), "Switch between Sessions and Browse".to_owned()),
                ],
                rect: Rect { x: 12.4, y: 30.6, w: 100.2, h: 28.9 },
                element_id: Some("new-session".to_owned()),
                view: Some("baaz::app::Harness".to_owned()),
            }),
            "Please make this\nless shouty.".to_owned(),
            context,
            None,
            None,
        );
        let mut settings = ScreenContext { mode: "settings".to_owned(), ..test_context() };
        settings.settings_section = Some("Providers".to_owned());
        draft.add(
            Target::Area { rect: Rect { x: 0.0, y: 0.0, w: 1440.0, h: 900.0 }, region: Some("composer".to_owned()) },
            "single line".to_owned(),
            settings,
            None,
            None,
        );
        (draft, ended)
    }

    #[test]
    fn markdown_matches_the_golden_string() {
        let (draft, ended) = golden_draft();
        let shots = vec![
            ("shots/note-1-crop.png".to_owned(), "shots/note-1-full.png".to_owned()),
            ("shots/note-2-crop.png".to_owned(), "shots/note-2-full.png".to_owned()),
        ];
        let rendered = render_markdown(&draft, ended, "0.1.0-test", &shots);
        let expected = format!(
            "# Baaz feedback — 2026-09-30 09:05\n\
             \n\
             > How to use this report: each note names what the owner pointed at, the screen it was on, and their words. Open the crop first; the full shot shows surroundings. `Code id` is a gpui element id you can grep for.\n\
             \n\
             | Field | Value |\n\
             |---|---|\n\
             | Build | 0.1.0-test |\n\
             | Window | 1440 × 900 @2x · dark |\n\
             | Started | {} |\n\
             | Ended | {} |\n\
             | Notes | 2 |\n\
             \n\
             ## Note 1 · New session · browser\n\
             Screen: sessions · project baaz · session \"Fix the crash\" (abc) · right pane browser (https://example.com)\n\
             Element: `New session` (Button) in `Sidebar` › `Switch between Sessions and Browse`\n\
             Code id: `new-session` (baaz::app::Harness)\n\
             Rect: `x=12 y=31 100 × 29`\n\
             Note:\n\
             > Please make this\n\
             > less shouty.\n\
             Shots: `shots/note-1-crop.png` · `shots/note-1-full.png`\n\
             \n\
             ## Note 2 · Area · composer\n\
             Screen: settings · Settings → Providers\n\
             Rect: `x=0 y=0 1440 × 900`\n\
             Note:\n\
             > single line\n\
             Shots: `shots/note-2-crop.png` · `shots/note-2-full.png`\n",
            iso(&draft.started),
            iso(&ended),
        );
        assert_eq!(rendered, expected);
        assert!(rendered.ends_with('\n') && !rendered.ends_with("\n\n"));
    }

    #[test]
    fn an_empty_draft_prints_no_notes() {
        let started = Local.with_ymd_and_hms(2026, 9, 30, 9, 5, 7).unwrap();
        let ended = Local.with_ymd_and_hms(2026, 9, 30, 9, 6, 0).unwrap();
        let draft = Draft { started, notes: Vec::new() };
        let rendered = render_markdown(&draft, ended, "0.1.0-test", &[]);
        let expected = format!(
            "# Baaz feedback — 2026-09-30 09:05\n\
             \n\
             > How to use this report: each note names what the owner pointed at, the screen it was on, and their words. Open the crop first; the full shot shows surroundings. `Code id` is a gpui element id you can grep for.\n\
             \n\
             | Field | Value |\n\
             |---|---|\n\
             | Build | 0.1.0-test |\n\
             | Window | unknown |\n\
             | Started | {} |\n\
             | Ended | {} |\n\
             | Notes | 0 |\n\
             \n\
             No notes.\n",
            iso(&started),
            iso(&ended),
        );
        assert_eq!(rendered, expected);
    }

    #[test]
    fn json_carries_the_schema_and_the_notes_fields() {
        let (draft, ended) = golden_draft();
        let json = render_json(&draft, ended, "0.1.0-test");
        assert_eq!(json["schema"], "baaz-feedback/1");
        assert_eq!(json["build"], "0.1.0-test");
        assert_eq!(json["notes"].as_array().unwrap().len(), 2);
        let first = &json["notes"][0];
        assert_eq!(first["index"], 1);
        assert_eq!(first["target"]["kind"], "element");
        assert_eq!(first["target"]["label"], "New session");
        assert_eq!(first["target"]["human_role"], "Button");
        assert_eq!(first["target"]["element_id"], "new-session");
        assert_eq!(first["target"]["view"], "baaz::app::Harness");
        assert_eq!(first["target"]["ancestors"].as_array().unwrap().len(), 2);
        let rect_x = first["rect"]["x"].as_f64().expect("rect x is a number");
        assert!((rect_x - 12.4).abs() < 1e-6, "rect x round-trips the f32");
        assert_eq!(first["text"], "Please make this\nless shouty.");
        assert_eq!(first["context"]["mode"], "sessions");
        assert_eq!(first["context"]["project"], "baaz");
        // The golden notes carry no images, so no shot is named.
        assert!(first["shots"]["crop"].is_null());
        assert!(first["shots"]["full"].is_null());
        let second = &json["notes"][1];
        assert_eq!(second["target"]["kind"], "area");
        assert_eq!(second["target"]["region"], "composer");
        assert_eq!(second["context"]["settings_section"], "Providers");
    }

    /// A scratch root only this test uses: `tempfile` is not a dependency,
    /// so the suite builds its own unique directory.
    fn scratch_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "baaz-feedback-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn imaged_draft() -> Draft {
        let (mut draft, _) = golden_draft();
        for note in draft.notes.iter_mut() {
            note.crop = Some(image::RgbaImage::from_pixel(36, 32, image::Rgba([1, 2, 3, 255])));
            note.full = Some(image::RgbaImage::from_pixel(40, 30, image::Rgba([4, 5, 6, 255])));
        }
        draft
    }

    #[test]
    fn untrusted_labels_cannot_break_the_markdown() {
        let (mut draft, ended) = golden_draft();
        if let Target::Element(hit) = &mut draft.notes[0].target {
            hit.label = "Run `cargo`\nnow".to_owned();
            hit.ancestors = vec![("AXGroup".to_owned(), String::new())];
        }
        let shots = vec![(String::new(), String::new()); 2];
        let md = render_markdown(&draft, ended, "baaz | dev", &shots);
        assert!(md.contains("| Build | baaz \\| dev |"), "pipe escaped in the table:\n{md}");
        assert!(md.contains("## Note 1 · Run 'cargo' now"), "label folded to one heading line:\n{md}");
        assert!(md.contains("Element: `Run 'cargo' now` (Button) in `(unnamed)`"), "{md}");
        assert!(md.contains("Shots: none"), "no shot named when none exist:\n{md}");
        assert!(!md.contains("note-1-crop"), "{md}");
    }

    #[test]
    fn a_note_without_images_names_no_shots_and_writes_none() {
        let root = scratch_root("noimages");
        let (draft, ended) = golden_draft();
        let dir = write_report(&root, &draft, ended, "0.1.0-test").expect("writes");
        let md = std::fs::read_to_string(dir.join("report.md")).expect("md");
        assert!(md.contains("Shots: none"), "{md}");
        assert!(!dir.join("shots/note-1-crop.png").exists());
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("report.json")).unwrap()).unwrap();
        assert!(json["notes"][0]["shots"]["crop"].is_null());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_report_lays_out_files_and_latest() {
        let root = scratch_root("layout");
        let draft = imaged_draft();
        let ended = Local.with_ymd_and_hms(2026, 9, 30, 9, 41, 22).unwrap();
        let dir = write_report(&root, &draft, ended, "0.1.0-test").expect("writes");
        assert_eq!(dir, root.join("2026-09-30-094122"));
        for name in ["report.md", "report.json"] {
            assert!(dir.join(name).is_file(), "{name} exists");
        }
        for n in [1, 2] {
            for kind in ["crop", "full"] {
                let path = dir.join(format!("shots/note-{n}-{kind}.png"));
                assert!(path.is_file(), "{} exists", path.display());
            }
        }
        // Full shots land at logical pixels (40×30 at 2× → 20×15).
        let full = image::open(dir.join("shots/note-1-full.png")).expect("decodes").to_rgba8();
        assert_eq!((full.width(), full.height()), (20, 15));
        let latest = std::fs::read_to_string(root.join("latest")).expect("latest");
        assert_eq!(latest, format!("{}\n", dir.display()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_second_write_in_the_same_second_gains_a_suffix() {
        let root = scratch_root("collision");
        let draft = imaged_draft();
        let ended = Local.with_ymd_and_hms(2026, 9, 30, 9, 41, 22).unwrap();
        let first = write_report(&root, &draft, ended, "0.1.0-test").expect("first");
        let second = write_report(&root, &draft, ended, "0.1.0-test").expect("second");
        assert_eq!(first, root.join("2026-09-30-094122"));
        assert_eq!(second, root.join("2026-09-30-094122-2"));
        let latest = std::fs::read_to_string(root.join("latest")).expect("latest");
        assert_eq!(latest, format!("{}\n", second.display()));
        let _ = std::fs::remove_dir_all(&root);
    }
}
