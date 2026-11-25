//! The check Baaz runs (docs/85 14.5b, D-Q8): found, not typed. One runner,
//! shared by orchestration's verdict (14.5) and goals (Stage 15).
//!
//! [`detect`] reads the folder for its test command, in this order: the
//! project's own `.baaz/project.toml` `check`, a `package.json` `test`
//! script (with the package manager its lock file names), `Cargo.toml`,
//! Python (`pyproject.toml` / `pytest.ini` / `setup.cfg` with pytest, or a
//! `tests/` folder of `test_*.py`), `go.mod`, a `Makefile` `test:` target.
//! Nothing found means [`Check::Parse`]: a commit exists and the changed
//! files parse.
//!
//! [`run`] runs a command in a folder with a deadline and keeps the tail of
//! its output: what a failing check sends back.

use std::path::Path;
use std::time::{Duration, Instant};

/// What a check is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    /// A shell command that must exit 0.
    Command(String),
    /// No command found: the changed files must parse.
    Parse,
}

impl Check {
    pub fn describe(&self) -> String {
        match self {
            Self::Command(command) => format!("`{command}` exits 0"),
            Self::Parse => "the changed files parse".into(),
        }
    }
}

/// Find the folder's check (see the module doc).
pub fn detect(folder: &Path) -> Check {
    let read = |name: &str| std::fs::read_to_string(folder.join(name)).ok();
    if let Some(config) = read(".baaz/project.toml") {
        if let Some(check) = toml_string(&config, "check").or_else(|| toml_string(&config, "test")) {
            return Check::Command(check);
        }
    }
    if let Some(package) = read("package.json") {
        let has_test = serde_json::from_str::<serde_json::Value>(&package)
            .ok()
            .and_then(|v| v.pointer("/scripts/test").and_then(|t| t.as_str()).map(str::to_owned))
            .is_some_and(|t| !t.contains("no test specified"));
        if has_test {
            let runner = if folder.join("pnpm-lock.yaml").exists() {
                "pnpm"
            } else if folder.join("yarn.lock").exists() {
                "yarn"
            } else if folder.join("bun.lockb").exists() || folder.join("bun.lock").exists() {
                "bun"
            } else {
                "npm"
            };
            return Check::Command(format!("{runner} test"));
        }
    }
    if folder.join("Cargo.toml").exists() {
        return Check::Command("cargo test".into());
    }
    let pytest_config = read("pytest.ini").is_some()
        || read("pyproject.toml").is_some_and(|t| t.contains("pytest"))
        || read("setup.cfg").is_some_and(|t| t.contains("[tool:pytest]"));
    let python_tests = std::fs::read_dir(folder.join("tests"))
        .map(|entries| entries.flatten().any(|e| e.file_name().to_string_lossy().starts_with("test_")))
        .unwrap_or(false)
        || std::fs::read_dir(folder).map(|entries| entries.flatten().any(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with("test_") && name.ends_with(".py")
        })).unwrap_or(false);
    if pytest_config || python_tests {
        return Check::Command("pytest -q".into());
    }
    if folder.join("go.mod").exists() {
        return Check::Command("go test ./...".into());
    }
    if read("Makefile").is_some_and(|m| m.lines().any(|line| line.starts_with("test:"))) {
        return Check::Command("make test".into());
    }
    Check::Parse
}

/// A `key = "value"` line in a small TOML file (the project file keeps
/// strings only).
fn toml_string(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').to_owned()).filter(|v| !v.is_empty())
    })
}

/// How a run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub passed: bool,
    /// The exit code; `None` when killed at the deadline or never started.
    pub code: Option<i32>,
    /// The last lines of stdout and stderr together.
    pub tail: String,
    pub seconds: u64,
}

/// Lines of output an outcome keeps.
pub const TAIL_LINES: usize = 60;

/// Run `command` through the shell in `folder` until it ends or `deadline`
/// passes (then it is killed, and fails). Blocks; off the UI thread. Never
/// run under test against a real project: tests point it at temp folders.
pub fn run(folder: &Path, command: &str, deadline: Duration) -> Outcome {
    let started = Instant::now();
    let child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("{command} 2>&1"))
        .current_dir(folder)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => return Outcome { passed: false, code: None, tail: format!("could not start `{command}`: {error}"), seconds: 0 },
    };
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(out) = stdout.as_mut() {
            use std::io::Read as _;
            let _ = out.read_to_string(&mut text);
        }
        text
    });
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) if started.elapsed() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break None,
        }
    };
    let text = reader.join().unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let mut tail = lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n");
    if code.is_none() {
        tail.push_str(&format!("\n(stopped after {} s)", deadline.as_secs()));
    }
    Outcome { passed: code == Some(0), code, tail, seconds: started.elapsed().as_secs() }
}

/// Whether each changed file parses, by its kind (JSON, TOML, YAML-free:
/// the formats a parse can be checked for without a toolchain); other kinds
/// pass. `Err` names the first file that does not.
pub fn parse_files(folder: &Path, files: &[String]) -> Result<(), String> {
    for file in files {
        let path = folder.join(file);
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let ok = match path.extension().and_then(|e| e.to_str()) {
            Some("json") => serde_json::from_str::<serde_json::Value>(&text).map(drop).map_err(|e| e.to_string()),
            Some("toml") => toml_check(&text),
            _ => Ok(()),
        };
        ok.map_err(|why| format!("{file} does not parse: {why}"))?;
    }
    Ok(())
}

/// A light TOML check: every non-blank line is a table header, a key =
/// value, a comment, or inside a multi-line string/array.
fn toml_check(text: &str) -> Result<(), String> {
    let mut depth = 0i32;
    for (i, line) in text.lines().enumerate() {
        let t = line.trim();
        if depth > 0 || t.is_empty() || t.starts_with('#') || t.starts_with('[') || t.contains('=') {
            depth += t.matches('[').count() as i32 - t.matches(']').count() as i32;
            depth = depth.max(0);
            continue;
        }
        return Err(format!("line {} is not TOML", i + 1));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(files: &[(&str, &str)]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("baaz-checks-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        for (name, text) in files {
            let path = dir.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// docs/85 14.5b: detection on fixture repos.
    #[test]
    fn the_check_is_found_in_each_kind_of_project() {
        let cases: Vec<(Vec<(&str, &str)>, Check)> = vec![
            (vec![("package.json", r#"{"scripts":{"test":"vitest run"}}"#)], Check::Command("npm test".into())),
            (vec![("package.json", r#"{"scripts":{"test":"jest"}}"#), ("pnpm-lock.yaml", "")], Check::Command("pnpm test".into())),
            (vec![("package.json", r#"{"scripts":{"test":"echo \"Error: no test specified\" && exit 1"}}"#)], Check::Parse),
            (vec![("Cargo.toml", "[package]\nname = \"x\"\n")], Check::Command("cargo test".into())),
            (vec![("pyproject.toml", "[tool.pytest.ini_options]\n")], Check::Command("pytest -q".into())),
            (vec![("tests/test_shop.py", "def test_x(): pass\n")], Check::Command("pytest -q".into())),
            (vec![("go.mod", "module x\n")], Check::Command("go test ./...".into())),
            (vec![("Makefile", "build:\n\ttrue\ntest:\n\ttrue\n")], Check::Command("make test".into())),
            (vec![(".baaz/project.toml", "run = \"npm run dev\"\ncheck = \"just ci\"\n"), ("Cargo.toml", "")], Check::Command("just ci".into())),
            (vec![("README.md", "# nothing to run\n")], Check::Parse),
        ];
        for (files, want) in cases {
            let dir = fixture(&files);
            assert_eq!(detect(&dir), want, "{files:?}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn the_runner_keeps_the_tail_and_the_verdict() {
        let dir = fixture(&[]);
        let ok = run(&dir, "echo one; echo two", Duration::from_secs(10));
        assert!(ok.passed && ok.tail.ends_with("two"), "{ok:?}");
        let failed = run(&dir, "for i in $(seq 1 100); do echo line $i; done; exit 3", Duration::from_secs(10));
        assert_eq!((failed.passed, failed.code), (false, Some(3)));
        assert_eq!(failed.tail.lines().count(), TAIL_LINES);
        assert!(failed.tail.ends_with("line 100"));
        let slow = run(&dir, "sleep 5", Duration::from_millis(300));
        assert!(!slow.passed && slow.code.is_none() && slow.tail.contains("stopped after"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn changed_files_must_parse_when_no_command_was_found() {
        let dir = fixture(&[("good.json", "{\"a\":1}"), ("bad.json", "{\"a\":"), ("c.toml", "[x]\na = 1\n"), ("notes.md", "anything")]);
        assert!(parse_files(&dir, &["good.json".into(), "c.toml".into(), "notes.md".into()]).is_ok());
        assert!(parse_files(&dir, &["bad.json".into()]).unwrap_err().starts_with("bad.json does not parse"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
