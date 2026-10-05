//! F2 feedback mode shell: the wiring contracts the unit tests cannot see.
//!
//! baaz is a binary, so this suite cannot drive its internals (key routing,
//! the Done round-trip, the overlay order and the step dispatch are pinned
//! by `#[gpui::test]` tests beside the code in `feedback/ui.rs`, plus the
//! `browser_visible` unit tests for the cover predicate). What this suite
//! pins is the shipped wiring, by *executing* the artifacts rather than
//! reading their text: the probe module loads at runtime and its entry
//! table — the data structure a probe run actually dispatches on — drives
//! the renamed `feedback-mode:` window verb onto the bar and the pill, with
//! no bare `feedback:` window step left to shadow the session verb.

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
print(json.dumps({"bar": module.ENTRIES["feedback-bar"], "pill": module.ENTRIES["feedback-pill"], "all": module.ENTRIES}))
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

/// The bar entry enters Active through the renamed window verb, and the
/// pill entry pauses through it: exact `--steps` payloads, read off the
/// runtime table.
#[test]
fn probe_entries_drive_feedback_mode_steps() {
    let entries = probe_entries();
    let bar = shot_steps(&entries["bar"]);
    assert!(
        bar.contains(&"--steps".to_owned()) && bar.contains(&"feedback-mode:on".to_owned()),
        "feedback-bar drives feedback-mode:on, got: {bar:?}"
    );
    let pill = shot_steps(&entries["pill"]);
    assert!(
        pill.contains(&"feedback-mode:on;feedback-mode:pause".to_owned()),
        "feedback-pill drives on then pause, got: {pill:?}"
    );
}

/// No probe step uses the bare `feedback:` window verb (it would shadow
/// the session verb's feedback/clarify field fill), and every
/// `feedback-mode:` sub is one the shell handles.
#[test]
fn no_entry_uses_the_shadowed_bare_feedback_verb() {
    let entries = probe_entries();
    let all = entries["all"].as_object().expect("ENTRIES is a table");
    let subs = ["on", "pause", "resume", "off", "discard", "done", "cancel"];
    for (name, entry) in all {
        let kinds = entry.as_object().unwrap_or_else(|| panic!("{name} is an entry"));
        for kind in ["shot", "idle"] {
            let Some(args) = kinds.get(kind).and_then(|args| args.as_array()) else {
                continue;
            };
            let mut steps: Vec<String> = Vec::new();
            let mut args = args.iter();
            while let Some(arg) = args.next() {
                if arg.as_str() == Some("--steps") {
                    if let Some(payload) = args.next().and_then(|arg| arg.as_str()) {
                        steps.extend(payload.split(';').map(str::to_owned));
                    }
                }
            }
            for step in steps {
                let (verb, rest) = step.split_once(':').unwrap_or((step.as_str(), ""));
                assert_ne!(
                    verb, "feedback",
                    "{name} still drives the shadowed bare verb: {step}"
                );
                if verb == "feedback-mode" {
                    let rest = rest.trim();
                    let pointing = rest.starts_with("hover:")
                        || rest.starts_with("click:")
                        || rest.starts_with("drag:")
                        || rest.starts_with("note:");
                    assert!(
                        subs.contains(&rest) || pointing,
                        "{name} drives an unknown sub: {step}"
                    );
                }
            }
        }
    }
}
