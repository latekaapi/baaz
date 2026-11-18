//! Probe truth, part 2 (task 4.10, audit A R7 / F20): the ratchet.
//!
//! The capture and scripting surface lives behind the `probe` cargo
//! feature, and what a capture changes about a run is one injected value,
//! `crate::runtime::Runtime`, not a check scattered through the code:
//!
//! * no `deterministic()` call anywhere outside `runtime.rs` (in the core
//!   crate since docs/82 4.11; the core crates' sources are read too), and no
//!   `scripted_chrome()` at all — a scripted run opens its lanes through
//!   the same open path as a real one, with a scripted factory in it;
//! * `steps`, `shot`, `bench`, `perf_budget`, `perf_turn` and the three
//!   per-feature step files are compiled only with `probe`, and every
//!   `fn step_*` outside them is too;
//! * gpui's `test-support` comes in only through `probe` (and the test
//!   build's dev-dependency), and the feature is on by default so every
//!   dev command keeps working;
//! * the release bundle turns the default features off, and the check
//!   build turns the probe back on.
//!
//! These pins read source text; they say nothing about behaviour, which the
//! unit tests and a `--no-default-features` build cover.

use std::path::{Path, PathBuf};

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repo_root() -> PathBuf {
    crate_root().join("..").join("..")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Every `.rs` file under `src/` and under the two core crates'
/// (`crates/baaz-core/src`, `crates/baaz-browser-core/src`, docs/82 4.11),
/// as `(path relative to src, text)`; a core file's path starts with its
/// crate's name.
fn sources() -> Vec<(String, String)> {
    let mut all = sources_under(&crate_root().join("src"), "");
    all.extend(sources_under(&crate_root().join("../baaz-core/src"), "baaz-core/"));
    all.extend(sources_under(&crate_root().join("../baaz-browser-core/src"), "baaz-browser-core/"));
    all
}

/// Every `.rs` file under `src`, as `(prefix + path relative to src, text)`.
fn sources_under(src: &Path, prefix: &str) -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("src reads").flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(src, &mut files);
    files.sort();
    files
        .into_iter()
        .map(|path| {
            let rel = path.strip_prefix(src).expect("under src").to_string_lossy().replace('\\', "/");
            (format!("{prefix}{rel}"), read(&path))
        })
        .collect()
}

/// The files compiled only with `probe`, as their module declarations gate
/// them.
const PROBE_MODULES: [(&str, &str); 8] = [
    ("main.rs", "mod bench;"),
    ("main.rs", "mod perf_budget;"),
    ("main.rs", "mod perf_turn;"),
    ("main.rs", "mod shot;"),
    ("main.rs", "mod steps;"),
    ("session.rs", "mod scripting;"),
    ("app/sessions/mod.rs", "mod steps;"),
    ("browser/mod.rs", "mod steps;"),
];

/// The files those declarations name, relative to `src/`.
const PROBE_FILES: [&str; 8] = [
    "bench.rs",
    "perf_budget.rs",
    "perf_turn.rs",
    "shot.rs",
    "steps.rs",
    "session/scripting.rs",
    "app/sessions/steps.rs",
    "browser/steps.rs",
];

/// Whether the item starting at line `at` carries `#[cfg(feature = "probe")]`
/// among the attributes and doc comments right above it.
fn gated(lines: &[&str], at: usize) -> bool {
    let mut i = at;
    while i > 0 {
        let above = lines[i - 1].trim();
        if above == "#[cfg(feature = \"probe\")]" {
            return true;
        }
        if above.starts_with("///") || above.starts_with("#[") || above.starts_with("//") {
            i -= 1;
            continue;
        }
        return false;
    }
    false
}

/// The lines of `text` before its unit-test module (tests always build with
/// the default features, so they may name the probe freely).
fn non_test_lines(text: &str) -> Vec<&str> {
    let lines: Vec<&str> = text.lines().collect();
    let end = lines
        .iter()
        .position(|line| {
            let line = line.trim();
            line.starts_with("mod tests") || (line.starts_with("mod ") && line.contains("tests {"))
        })
        .unwrap_or(lines.len());
    lines[..end].to_vec()
}

#[test]
fn no_deterministic_check_lives_outside_the_runtime() {
    let mut hits = Vec::new();
    for (rel, text) in sources() {
        if rel == "baaz-core/runtime.rs" {
            continue;
        }
        for (n, line) in text.lines().enumerate() {
            if line.contains("deterministic()") || line.contains("scripted_chrome()") {
                hits.push(format!("{rel}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    assert!(hits.is_empty(), "a capture is a property of the runtime, not a check:\n{}", hits.join("\n"));
    let runtime = read(&crate_root().join("../baaz-core/src/runtime.rs"));
    assert!(!runtime.contains("scripted_chrome()"), "no scripted-chrome check, even in the runtime");
}

#[test]
fn the_probe_modules_compile_only_with_the_probe() {
    let src = crate_root().join("src");
    for (file, decl) in PROBE_MODULES {
        let text = read(&src.join(file));
        let lines: Vec<&str> = text.lines().collect();
        let at = lines
            .iter()
            .position(|line| line.trim() == decl)
            .unwrap_or_else(|| panic!("{file} declares `{decl}`"));
        assert!(gated(&lines, at), "{file}: `{decl}` is behind #[cfg(feature = \"probe\")]");
    }
    for file in PROBE_FILES {
        assert!(src.join(file).is_file(), "{file} exists (rename it here too)");
    }
}

#[test]
fn every_step_verb_handler_is_probe_only() {
    let mut open = Vec::new();
    for (rel, text) in sources() {
        if PROBE_FILES.contains(&rel.as_str()) || rel.contains("/tests/") {
            continue;
        }
        let lines = non_test_lines(&text);
        for (n, line) in lines.iter().enumerate() {
            if line.contains("fn step_") && !gated(&lines, n) {
                open.push(format!("{rel}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    assert!(
        open.is_empty(),
        "a `step_*` function is a probe verb: gate it, or name production code for what it does:\n{}",
        open.join("\n")
    );
}

#[test]
fn test_support_comes_only_through_the_probe() {
    let manifest = read(&crate_root().join("Cargo.toml"));
    let section = |name: &str| -> String {
        let start = manifest.find(&format!("[{name}]")).unwrap_or_else(|| panic!("[{name}] exists"));
        let rest = &manifest[start + name.len() + 2..];
        let end = rest.find("\n[").unwrap_or(rest.len());
        rest[..end].to_owned()
    };
    let features = section("features");
    assert!(features.contains("default = [\"probe\"]"), "the probe is on by default: every dev command keeps it");
    let probe = features.lines().find(|line| line.starts_with("probe = ")).expect("a probe feature");
    for wanted in ["gpui/test-support", "gpui_platform/test-support"] {
        assert!(probe.contains(wanted), "`probe` turns on {wanted}");
    }
    // The probe is the only way in: no other feature names test-support
    // (Feedback Mode's `window-capture` went with it, plan 4.11b), so the
    // shipped app carries none of it.
    for line in features.lines().filter(|line| !line.trim_start().starts_with('#')) {
        if line.contains("test-support") {
            assert!(line.starts_with("probe = "), "test-support outside the probe: `{line}`");
        }
    }
    assert!(!features.contains("window-capture"), "no `window-capture` feature exists");
    // The core crate's own probe (on by default for its tests) reaches the
    // app only through this crate's `probe`: the workspace pins it with no
    // default features (docs/82 4.11).
    assert!(probe.contains("baaz-core/probe"), "`probe` turns on the core's probe");
    let workspace = read(&repo_root().join("Cargo.toml"));
    let core_entry = workspace
        .lines()
        .find(|line| line.starts_with("baaz-core = "))
        .expect("the workspace names baaz-core");
    assert!(core_entry.contains("default-features = false"), "baaz-core comes in without its default probe: `{core_entry}`");
    let dependencies = section("dependencies");
    assert!(
        !dependencies.contains("test-support"),
        "no dependency turns on test-support unconditionally: the release build must not carry it"
    );
}

#[test]
fn the_release_bundle_builds_without_the_probe() {
    let bundle = read(&repo_root().join("scripts/bundle.sh"));
    let builds: Vec<&str> = bundle
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("cargo build --release -p baaz"))
        .collect();
    assert!(!builds.is_empty(), "bundle.sh builds baaz");
    for build in builds {
        assert!(
            build.contains("--no-default-features"),
            "every release build of baaz in bundle.sh drops the default probe: `{build}`"
        );
        for named in ["test-support", "window-capture", "baaz/probe"] {
            assert!(!build.contains(named), "a release build names no test-support feature ({named}): `{build}`");
        }
    }
    assert!(
        bundle.contains("PROBE_FEATURE=\"baaz/probe\"") && bundle.contains("BAAZ_PROBE"),
        "only BAAZ_PROBE=1 puts the probe back"
    );
    let check = read(&repo_root().join("scripts/baaz-check.sh"));
    assert!(check.contains("BAAZ_PROBE=1"), "the check build is driven by --steps: it keeps the probe");
}

#[test]
fn a_build_without_the_probe_refuses_probe_runs_loudly() {
    let main = read(&crate_root().join("src/main.rs"));
    for flag in ["\"--steps\"", "\"--screenshot\"", "\"--replay\"", "\"--bench\"", "\"--perf-turn\"", "\"--no-connect\""] {
        let listed = main
            .split("const PROBE_FLAGS")
            .nth(1)
            .and_then(|rest| rest.split("];").next())
            .is_some_and(|list| list.contains(flag));
        assert!(listed, "{flag} is refused by name without the probe");
    }
    assert!(
        main.contains("is a probe flag, and this build has no probe"),
        "the refusal says why"
    );
    assert!(
        main.contains("BAAZ_DETERMINISTIC=1 is a probe run, and this build has no probe"),
        "a capture's environment is refused too, never run as a person's app"
    );
}
