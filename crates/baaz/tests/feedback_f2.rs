//! F2 feedback mode shell: the wiring contracts the unit tests cannot see.
//!
//! Like `modes_h2.rs`, this suite cannot boot the app (baaz is a
//! binary), so it pins the wiring in source text: the `feedback_mode`
//! setting and its General switch, the `ToggleFeedback` action with its
//! ⌃⌥⌘F row and Help menu item, the `BaazFeedback` Escape row, the capture
//! layer's id and context, the bar and pill ids with their labelled
//! buttons, the `feedback_cover` browser predicate, the `feedback:`
//! step verb and its doc row, and the two probe entries. Each assertion
//! fails on the pre-F2 code. The transition table, the serde default,
//! the keymap rows, the cover predicate and the Done round-trip run as
//! unit tests beside the code they cover.

use std::path::{Path, PathBuf};

fn manifest() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn source(name: &str) -> String {
    let path = manifest().join("src").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("source reads: {}", path.display()))
}

fn repo_file(name: &str) -> String {
    let path = manifest().join("../../").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("source reads: {}", path.display()))
}

/// The setting: `Layout.feedback_mode`, serde default false, the General
/// switch with its id, label and detail, and the live-apply that parks
/// the shell Off when flipped off.
#[test]
fn feedback_setting_defaults_off_with_a_general_switch() {
    let layout = source("layout.rs");
    assert!(layout.contains("pub feedback_mode: bool"), "the Layout field exists");
    assert!(
        layout.contains("\"feedbackMode\""),
        "the serde rename is feedbackMode"
    );
    let settings = source("settings.rs");
    assert!(
        settings.contains("\"feedback_mode\" => layout.feedback_mode = on"),
        "apply_setting flips it by row id"
    );
    assert!(
        settings.contains("| \"feedback_mode\" => Some(\"general\")")
            || settings.contains("\"feedback_mode\" => Some(\"general\")"),
        "its home page is General"
    );
    assert!(
        settings.contains("id: SharedString::from(\"feedback_mode\")"),
        "the General switch row exists"
    );
    assert!(settings.contains("Feedback mode"), "the switch label reads Feedback mode");
    assert!(
        settings.contains("⌃⌥⌘F points at parts of Baaz to leave notes for Claude. Saves a report folder."),
        "the switch detail names the gesture and the report"
    );
    assert!(
        settings.contains("self.feedback_setting_off(cx)"),
        "flipping the setting off parks the shell"
    );
}

/// The action, its ⌃⌥⌘F root row, the layer-scoped Escape row, the
/// binding arms, the root handlers and the Help menu item.
#[test]
fn toggle_feedback_is_an_action_bound_paused_and_menued() {
    let app = source("app.rs");
    assert!(app.contains("ToggleFeedback,"), "the action is declared");
    assert!(app.contains("FeedbackEscape,"), "the escape action is declared");
    assert!(
        app.contains("this.toggle_feedback(window, cx)"),
        "the window root handles the toggle"
    );
    assert!(
        app.contains("this.escape_feedback(window, cx)"),
        "the window root handles the layer escape"
    );
    assert!(
        app.contains("gpui::MenuItem::action(\"Feedback Mode\", ToggleFeedback)"),
        "the Help menu carries it"
    );
    let keymap = source("keymap.rs");
    assert!(
        keymap.contains("\"ToggleFeedback\", keystroke: \"ctrl-alt-cmd-f\""),
        "the ⌃⌥⌘F row exists"
    );
    assert!(
        keymap.contains("Some(aui::keys::ROOT_CONTEXT)"),
        "a root-context row exists for the pre-key router"
    );
    assert!(
        keymap.contains("\"FeedbackEscape\", keystroke: \"escape\""),
        "the layer Escape row exists"
    );
    assert!(
        keymap.contains("feedback_ui::FEEDBACK_CONTEXT"),
        "the Escape row rides the layer context"
    );
    assert!(
        keymap.contains("\"ToggleFeedback\" => Some(gpui::KeyBinding::new("),
        "the toggle builds a binding"
    );
    assert!(
        keymap.contains("\"FeedbackEscape\" => Some(gpui::KeyBinding::new("),
        "the escape builds a binding"
    );
}

/// The capture layer: full-window, Active only, swallowing, with the
/// inset frame, crosshair, layer id and the `BaazFeedback` context — and
/// the focus parking that returns cleanly when it closes.
#[test]
fn capture_layer_blocks_active_only_with_an_f3_hook() {
    let ui = source("feedback/ui.rs");
    assert!(ui.contains(".id(\"feedback-layer\")"), "the layer has its id");
    assert!(ui.contains("FEEDBACK_CONTEXT"), "the layer carries its key context");
    assert!(ui.contains("stop_propagation"), "the layer swallows pointer events");
    assert!(ui.contains("Crosshair"), "the layer wears a crosshair");
    assert!(ui.contains("F3 hook"), "F3 finds where to attach");
    let app = source("app.rs");
    assert!(
        app.contains("self.feedback.phase != feedback_ui::FeedbackPhase::Off"),
        "focus parks when the shell closes"
    );
    assert!(
        app.contains(".children(feedback_layer)"),
        "the layer renders under the chrome"
    );
    assert!(
        app.contains(".children(feedback_chrome)"),
        "the bar/pill render above the layer"
    );
}

/// The bar (Active, top-centre group) and the pill (Paused,
/// bottom-right group): every button labelled, Done gated on notes.
#[test]
fn bar_and_pill_carry_labelled_buttons() {
    let ui = source("feedback/ui.rs");
    for id in ["feedback-bar", "feedback-pill"] {
        assert!(ui.contains(&format!(".id(\"{id}\")")), "the {id} exists");
    }
    assert!(ui.contains("aria_label(\"Feedback mode\")"), "the bar is a labelled group");
    for label in [
        "Pause feedback",
        "Discard feedback notes",
        "Done — save feedback report",
        "Resume feedback",
    ] {
        assert!(
            ui.contains(&format!("accessibility_label(\"{label}\")")),
            "the {label} button is named"
        );
    }
    assert!(ui.contains("Click an element or drag an area"), "the bar names the gesture");
    assert!(
        ui.contains("Saves the report and copies its path"),
        "Done says what it does"
    );
}

/// Done writes off the UI thread, copies the path, toasts, and parks Off
/// cleared — or keeps the draft on error.
#[test]
fn done_saves_off_thread_then_parks_off() {
    let ui = source("feedback/ui.rs");
    assert!(ui.contains("background_executor()"), "the report writes off the UI thread");
    assert!(ui.contains("write_report"), "Done goes through the F1 writer");
    assert!(ui.contains("feedback_root()"), "reports land under the feedback root");
    assert!(ui.contains("write_to_clipboard"), "the report path lands on the clipboard");
    assert!(ui.contains("Feedback saved ·"), "success toasts the note count");
    assert!(ui.contains("Couldn't save feedback"), "failure toasts and keeps the draft");
    assert!(ui.contains("done_applied"), "success parks Off with a cleared draft");
}

/// The browser predicate NANDs the Active-only cover, and the sync feeds
/// it from the shell's phase.
#[test]
fn feedback_cover_hides_pages_while_active_only() {
    let browser = source("browser/mod.rs");
    assert!(browser.contains("pub feedback_cover: bool"), "the predicate has its input");
    assert!(browser.contains("!inputs.feedback_cover"), "the predicate NANDs it");
    assert!(
        browser.contains("feedback_cover: self.feedback.covers_app()"),
        "the sync reads the shell phase"
    );
    let ui = source("feedback/ui.rs");
    assert!(
        ui.contains("self.phase == FeedbackPhase::Active"),
        "the cover is Active only — Paused pages stay live"
    );
}

/// The `feedback:` window verb with its six subs and doc row; any other
/// payload stays the session verb's field fill.
#[test]
fn feedback_step_verb_has_subs_and_a_doc_row() {
    let steps = source("steps.rs");
    assert!(
        steps.contains("verb: \"feedback\", run: |this, rest, window, cx| this.step_feedback(rest, window, cx)"),
        "the window owns the feedback verb"
    );
    assert!(
        steps.contains("`feedback:<on"),
        "the doc table names the mode subs"
    );
    let ui = source("feedback/ui.rs");
    for sub in ["\"on\"", "\"pause\"", "\"resume\"", "\"off\"", "\"discard\"", "\"done\""] {
        assert!(ui.contains(sub), "the step verb handles {sub}");
    }
    assert!(
        ui.contains("v.step_feedback(rest, window, cx)"),
        "other payloads forward to the session field"
    );
}

/// The two probe entries ride the new verb, offline and idempotent, with
/// no baselines checked in.
#[test]
fn probe_entries_cover_bar_and_pill() {
    let probe = repo_file("scripts/uiprobe.py");
    assert!(
        probe.contains("\"feedback-bar\""),
        "the Active bar has a probe entry"
    );
    assert!(
        probe.contains("\"feedback-pill\""),
        "the Paused pill has a probe entry"
    );
    assert!(
        probe.contains("\"--steps\", \"feedback:on\""),
        "the bar entry enters Active through the verb"
    );
    assert!(
        probe.contains("\"--steps\", \"feedback:on;feedback:pause\""),
        "the pill entry pauses through the verb"
    );
}
