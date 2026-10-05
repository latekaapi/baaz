//! B2a-H7b: links from other apps open in Baaz; "Make Baaz your default
//! browser".
//!
//! baaz is a binary, so this suite cannot drive its internals (the
//! routing itself — classification, queue-then-flush through the frame
//! hook, tab order, handler names, the confirm wait — is pinned by unit
//! and `#[gpui::test]` tests beside the code). What this suite pins is
//! the shipped wiring, by *executing* the artifacts rather than reading
//! their text: the bundle script still parses, the probe CLI still runs,
//! and the probe's `browse-opened-link` entry still drives the `open-url:`
//! step onto wikipedia when the module is loaded and its entry table
//! read at runtime.

use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("crates/").parent().expect("root/").to_path_buf()
}

/// The bundle script parses: `sh -n` executes the shell parser over it
/// and fails on any syntax error.
#[test]
fn bundle_script_parses() {
    let script = workspace_root().join("scripts").join("bundle.sh");
    let status = Command::new("sh")
        .arg("-n")
        .arg(&script)
        .status()
        .unwrap_or_else(|error| panic!("sh -n {}: {error}", script.display()));
    assert!(status.success(), "scripts/bundle.sh parses");
}

/// The probe CLI runs: `--help` exercises argument parsing and exits 0
/// with the `--entry` flag it drives captures through.
#[test]
fn probe_cli_runs() {
    let probe = workspace_root().join("scripts").join("uiprobe.py");
    let output = Command::new("python3")
        .arg(&probe)
        .arg("--help")
        .output()
        .unwrap_or_else(|error| panic!("python3 {} --help: {error}", probe.display()));
    assert!(output.status.success(), "uiprobe.py --help exits 0");
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("--entry"), "the probe CLI offers --entry");
}

/// The probe's `browse-opened-link` entry drives the `open-url:` step
/// onto wikipedia. Loads the module at runtime and reads the entry
/// table the probe itself dispatches on — the data structure a probe run
/// executes, not the file's text.
#[test]
fn probe_entry_drives_an_opened_link() {
    let probe = workspace_root().join("scripts").join("uiprobe.py");
    let loader = r#"
import importlib.util, json, sys
spec = importlib.util.spec_from_file_location("uiprobe", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
print(json.dumps(module.ENTRIES["browse-opened-link"]))
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
    let entry = String::from_utf8_lossy(&output.stdout);
    assert!(
        entry.contains("open-url:https://www.wikipedia.org/"),
        "browse-opened-link drives open-url: onto wikipedia, got: {entry}"
    );
}
