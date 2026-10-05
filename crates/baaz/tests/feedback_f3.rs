//! F3 feedback commenting: the wiring contracts the unit tests cannot see.
//!
//! baaz is a binary, so this suite cannot drive its internals (pointer
//! routing, the capture round-trip and the popover state machine are pinned
//! by `#[gpui::test]` tests beside the code in `feedback/ui.rs`). What this
//! suite pins is the shipped wiring, by *executing* the artifacts rather
//! than reading their text: the probe module loads at runtime and its entry
//! table — the data structure a probe run actually dispatches on — drives
//! the F3 `feedback-mode:` subs onto the hover outline, the note popover
//! and the pins, with no `wait:` in these window-only scripts.

use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("crates/").parent().expect("root/").to_path_buf()
}

/// The probe module's `ENTRIES` table, loaded at runtime and printed as
/// JSON: the table `boot()` dispatches on, not the file's text.
fn probe_entries() -> serde_json::Value {
    let probe = workspace_root().join("scripts").join("uiprobe.py");
    let loader = r#"
import importlib.util, json, sys
spec = importlib.util.spec_from_file_location("uiprobe", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
print(json.dumps({"hover": module.ENTRIES["feedback-hover"], "note": module.ENTRIES["feedback-note"], "pins": module.ENTRIES["feedback-pins"], "all": module.ENTRIES}))
"#;
    let output = Command::new("python3")
        .arg("-c")
        .arg(loader)
        .arg(&probe)
        .output()
        .unwrap_or_else(|error| panic!("loading {}: {error}", probe.display()));
    assert!(
        output.status.success(),
        "uiprobe.py loads: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("entries parse as JSON")
}

fn shot_steps(entry: &serde_json::Value) -> Vec<String> {
    entry
        .get("shot")
        .and_then(|shot| shot.as_array())
        .unwrap_or_else(|| panic!("entry has a shot list: {entry}"))
        .iter()
        .map(|arg| arg.as_str().unwrap_or_default().to_owned())
        .collect()
}

/// The exact `--steps` payload each entry passes to baaz.
fn steps_arg(entry: &serde_json::Value) -> String {
    let args = shot_steps(entry);
    let mut steps = String::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--steps" {
            steps = args.next().cloned().unwrap_or_default();
        }
    }
    assert!(!steps.is_empty(), "entry carries a --steps payload: {args:?}");
    steps
}

/// Every entry runs offline as a signed-in login, like the F2 shell entries.
fn assert_offline_signed_in(entry: &serde_json::Value, name: &str) {
    let args = shot_steps(entry);
    for flag in ["--no-connect", "--login", "signed-in"] {
        assert!(
            args.contains(&flag.to_owned()),
            "{name} runs {flag} offline: {args:?}"
        );
    }
}

/// The three F3 entries drive the exact scripted flows: hover names the
/// header's project button, note leaves its popover open, pins saves an
/// element note and an area note.
#[test]
fn probe_entries_drive_the_f3_flows() {
    let entries = probe_entries();
    assert_offline_signed_in(&entries["hover"], "feedback-hover");
    assert_offline_signed_in(&entries["note"], "feedback-note");
    assert_offline_signed_in(&entries["pins"], "feedback-pins");
    assert_eq!(
        steps_arg(&entries["hover"]),
        "feedback-mode:on;feedback-mode:hover:297,21",
        "feedback-hover names the project button"
    );
    assert_eq!(
        steps_arg(&entries["note"]),
        "feedback-mode:on;feedback-mode:click:297,21",
        "feedback-note clicks the project button and leaves the popover open"
    );
    assert_eq!(
        steps_arg(&entries["pins"]),
        "feedback-mode:on;feedback-mode:click:297,21;feedback-mode:note:Make this quieter;feedback-mode:drag:560,300,900,420;feedback-mode:note:Too much space here",
        "feedback-pins saves an element note and an area note"
    );
}

/// Window-only scripts never `wait:` for a session that never opens, and
/// every `feedback-mode:` sub they use is one the shell handles.
#[test]
fn f3_entries_use_only_handled_subs_and_no_wait() {
    let entries = probe_entries();
    for name in ["hover", "note", "pins"] {
        let entry = &entries[name];
        let payload = steps_arg(entry);
        for step in payload.split(';') {
            let (verb, rest) = step.split_once(':').unwrap_or((step, ""));
            assert_ne!(verb, "wait", "feedback-{name} scripts no wait: {step}");
            if verb == "feedback-mode" {
                let handled = ["on", "pause", "resume", "off", "discard", "done", "cancel"]
                    .contains(&rest.trim())
                    || rest.starts_with("hover:")
                    || rest.starts_with("click:")
                    || rest.starts_with("drag:")
                    || rest.starts_with("note:");
                assert!(handled, "feedback-{name} drives an unknown sub: {step}");
            }
        }
    }
}
