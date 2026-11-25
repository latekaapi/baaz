//! Claude Code's `PostToolUse` hook, run as `baaz --tool-output-hook`
//! (docs/85 12.2): the shared output filter
//! ([`provider::output_filter`]) applied to what a tool returned, handed
//! back as `updatedToolOutput` in the tool's own shape.
//!
//! Probed live on claude 2.1.276 (docs/85-evidence/12.2): the hook's stdin
//! carries `tool_name` and `tool_response`; an answer of
//! `{"hookSpecificOutput": {"hookEventName": "PostToolUse",
//! "updatedToolOutput": …}}` replaces what the model reads, for the
//! built-in tools too, when it matches the tool's shape (a mismatch is
//! ignored silently, so the shapes below are the probed ones):
//!
//! * Bash: `{stdout, stderr, interrupted, isImage, noOutputExpected}` —
//!   `stdout` and `stderr` go through the filter, every other field is
//!   passed back as it came.
//! * Read: `{type: "text", file: {filePath, content, numLines, startLine,
//!   totalLines}}` — only with `--read-stub`: a whole-file read of a file
//!   unchanged since this session's last read of it answers with a short
//!   stub instead of the content again. claude carried that stub itself
//!   until 2.1.317 ([`READ_STUB_FROM_VERSION`]), so before then the hook
//!   never adds a second one.
//!
//! The hook never fails a tool: anything it cannot read, it leaves alone
//! (no output, exit 0).

use serde_json::{json, Value};

/// The first claude that no longer answers a repeated Read with its own
/// "file unchanged" stub (removed in 2.1.317).
pub const READ_STUB_FROM_VERSION: &str = "2.1.317";

/// The flag that turns the Read stub on.
pub const READ_STUB_FLAG: &str = "--read-stub";

/// The early binary mode the hook runs as.
pub const HOOK_MODE: &str = "--tool-output-hook";

/// Whether claude `version` needs Baaz's Read stub.
pub fn wants_read_stub(version: Option<&str>) -> bool {
    version.is_some_and(|version| provider::version::compare_versions(version, READ_STUB_FROM_VERSION) != std::cmp::Ordering::Less)
}

/// The stub a repeated, unchanged read answers with.
pub const READ_STUB_TEXT: &str =
    "File unchanged since your last Read of it in this session; the content from that Read is still current.";

/// The `hooks` settings entry that runs `exe` as the hook for Bash (and
/// Read with the stub).
pub fn hook_settings(exe: &std::path::Path, read_stub: bool) -> serde_json::Map<String, Value> {
    let quoted = format!("'{}'", exe.to_string_lossy().replace('\'', r"'\''"));
    let command = if read_stub { format!("{quoted} {HOOK_MODE} {READ_STUB_FLAG}") } else { format!("{quoted} {HOOK_MODE}") };
    let matcher = if read_stub { "Bash|Read" } else { "Bash" };
    let mut settings = serde_json::Map::new();
    settings.insert(
        "hooks".into(),
        json!({"PostToolUse": [{"matcher": matcher, "hooks": [{"type": "command", "command": command}]}]}),
    );
    settings
}

/// The hook's answer to one call (its stdin), or `None` to leave the
/// tool's output as it is. `seen` remembers reads for the stub.
pub fn answer(input: &str, read_stub: bool, seen: &mut dyn ReadMemory) -> Option<String> {
    let call: Value = serde_json::from_str(input).ok()?;
    let response = call.get("tool_response")?;
    let updated = match call.get("tool_name")?.as_str()? {
        "Bash" => filtered_bash(response)?,
        "Read" if read_stub => stubbed_read(&call, response, seen)?,
        _ => return None,
    };
    Some(json!({"hookSpecificOutput": {"hookEventName": "PostToolUse", "updatedToolOutput": updated}}).to_string())
}

/// Bash's response with `stdout` and `stderr` filtered; `None` when
/// neither shrank.
fn filtered_bash(response: &Value) -> Option<Value> {
    let mut updated = response.as_object()?.clone();
    let mut changed = false;
    for field in ["stdout", "stderr"] {
        if let Some(filtered) = updated.get(field).and_then(Value::as_str).and_then(provider::output_filter::filter) {
            updated.insert(field.into(), Value::String(filtered.text));
            changed = true;
        }
    }
    changed.then_some(Value::Object(updated))
}

/// What the stub needs to remember between hook runs: the last whole read
/// of each file, per session (`(len, modified ns)`).
pub trait ReadMemory {
    /// The stamp recorded for `(session, path)`.
    fn get(&self, session: &str, path: &str) -> Option<(u64, u128)>;
    /// Record a stamp.
    fn put(&mut self, session: &str, path: &str, stamp: (u64, u128));
}

/// Read's response with the stub for content when the same session read
/// the whole of this unchanged file before.
fn stubbed_read(call: &Value, response: &Value, seen: &mut dyn ReadMemory) -> Option<Value> {
    let session = call.get("session_id")?.as_str()?;
    let input = call.get("tool_input")?;
    // Only a whole-file read: a slice is its own question.
    if input.get("offset").is_some_and(|v| !v.is_null()) || input.get("limit").is_some_and(|v| !v.is_null()) {
        return None;
    }
    let path = response.pointer("/file/filePath")?.as_str()?;
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    let stamp = (meta.len(), modified);
    let before = seen.get(session, path);
    seen.put(session, path, stamp);
    if before != Some(stamp) {
        return None;
    }
    let mut updated = response.clone();
    *updated.pointer_mut("/file/content")? = Value::String(READ_STUB_TEXT.into());
    Some(updated)
}

/// The memory a real hook run keeps: one small JSON file per session in the
/// temp folder (the hook is a fresh process per call).
pub struct TempReadMemory;

impl TempReadMemory {
    fn file(session: &str) -> std::path::PathBuf {
        let safe: String = session.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
        std::env::temp_dir().join("baaz-read-stub").join(format!("{safe}.json"))
    }
}

impl ReadMemory for TempReadMemory {
    fn get(&self, session: &str, path: &str) -> Option<(u64, u128)> {
        let text = std::fs::read_to_string(Self::file(session)).ok()?;
        let map: std::collections::HashMap<String, (u64, u128)> = serde_json::from_str(&text).ok()?;
        map.get(path).copied()
    }

    fn put(&mut self, session: &str, path: &str, stamp: (u64, u128)) {
        let file = Self::file(session);
        let mut map: std::collections::HashMap<String, (u64, u128)> =
            std::fs::read_to_string(&file).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
        map.insert(path.to_owned(), stamp);
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&file, serde_json::to_string(&map).unwrap_or_default());
    }
}

/// The hook run: stdin in, an answer (or nothing) out, always exit 0.
pub fn run(args: &[String]) {
    use std::io::{Read as _, Write as _};
    let read_stub = args.iter().any(|arg| arg == READ_STUB_FLAG);
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return;
    }
    if let Some(answer) = answer(&input, read_stub, &mut TempReadMemory) {
        let _ = std::io::stdout().write_all(answer.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Memory(std::collections::HashMap<(String, String), (u64, u128)>);

    impl ReadMemory for Memory {
        fn get(&self, session: &str, path: &str) -> Option<(u64, u128)> {
            self.0.get(&(session.to_owned(), path.to_owned())).copied()
        }
        fn put(&mut self, session: &str, path: &str, stamp: (u64, u128)) {
            self.0.insert((session.to_owned(), path.to_owned()), stamp);
        }
    }

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/output-filter").join(name)).unwrap()
    }

    /// The probed Bash shape comes back with the same fields, filtered.
    #[test]
    fn a_long_bash_output_comes_back_filtered_in_bashs_shape() {
        let log = fixture("cargo-test.log");
        let call = json!({"session_id": "s", "tool_name": "Bash", "tool_input": {"command": "cargo test"},
            "tool_response": {"stdout": log, "stderr": "", "interrupted": false, "isImage": false, "noOutputExpected": false}});
        let answer: Value = serde_json::from_str(&answer(&call.to_string(), false, &mut Memory::default()).expect("answered")).unwrap();
        assert_eq!(answer["hookSpecificOutput"]["hookEventName"], "PostToolUse");
        let updated = &answer["hookSpecificOutput"]["updatedToolOutput"];
        let mut keys: Vec<&String> = updated.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, ["interrupted", "isImage", "noOutputExpected", "stderr", "stdout"], "the tool's own shape");
        assert!(updated["stdout"].as_str().unwrap().contains("test tests::subtracts_wrongly ... FAILED"));
        assert!(updated["stdout"].as_str().unwrap().len() * 2 < log.len());
    }

    #[test]
    fn short_output_and_other_tools_are_left_alone() {
        let short = json!({"tool_name": "Bash", "tool_response": {"stdout": "hello", "stderr": ""}});
        assert_eq!(answer(&short.to_string(), true, &mut Memory::default()), None);
        let grep = json!({"tool_name": "Grep", "tool_response": {"content": fixture("cargo-test.log")}});
        assert_eq!(answer(&grep.to_string(), true, &mut Memory::default()), None);
        assert_eq!(answer("not json", true, &mut Memory::default()), None, "never fails a tool");
    }

    /// The Read stub: only with the flag (claude ≥ 2.1.317), only for a
    /// whole-file read of an unchanged file this session read before, and
    /// in Read's probed shape.
    #[test]
    fn a_repeated_unchanged_read_gets_the_stub_only_when_switched_on() {
        let dir = std::env::temp_dir().join(format!("baaz-read-stub-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("README.md");
        std::fs::write(&file, "first\nsecond\n").unwrap();
        let path = file.to_string_lossy().into_owned();
        let call = |session: &str, input: Value| {
            json!({"session_id": session, "tool_name": "Read", "tool_input": input,
                "tool_response": {"type": "text", "file": {"filePath": path, "content": "first\nsecond\n", "numLines": 3, "startLine": 1, "totalLines": 3}}})
            .to_string()
        };
        let whole = json!({"file_path": path});
        let mut memory = Memory::default();
        assert_eq!(answer(&call("s", whole.clone()), false, &mut memory), None, "off before 2.1.317: claude stubs itself");
        assert_eq!(answer(&call("s", whole.clone()), true, &mut memory), None, "the first read is the content");
        let stubbed: Value = serde_json::from_str(&answer(&call("s", whole.clone()), true, &mut memory).expect("stub")).unwrap();
        let updated = &stubbed["hookSpecificOutput"]["updatedToolOutput"];
        assert_eq!(updated["file"]["content"], READ_STUB_TEXT);
        assert_eq!(updated["type"], "text");
        assert_eq!(updated["file"]["totalLines"], 3, "the rest of Read's shape stays");
        assert_eq!(answer(&call("other", whole.clone()), true, &mut memory), None, "another session reads it fresh");
        assert_eq!(answer(&call("s", json!({"file_path": path, "offset": 2})), true, &mut memory), None, "a slice is never stubbed");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&file, "first\nchanged\n").unwrap();
        assert_eq!(answer(&call("s", whole), true, &mut memory), None, "a changed file is read again");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_settings_name_the_hook_and_the_version_switch() {
        let settings = hook_settings(std::path::Path::new("/Applications/Baaz.app/Contents/MacOS/Baaz"), false);
        assert_eq!(
            settings["hooks"]["PostToolUse"][0],
            json!({"matcher": "Bash", "hooks": [{"type": "command", "command": "'/Applications/Baaz.app/Contents/MacOS/Baaz' --tool-output-hook"}]})
        );
        let with_read = hook_settings(std::path::Path::new("/x/Baaz"), true);
        assert_eq!(with_read["hooks"]["PostToolUse"][0]["matcher"], "Bash|Read");
        assert!(with_read["hooks"]["PostToolUse"][0]["hooks"][0]["command"].as_str().unwrap().ends_with("--tool-output-hook --read-stub"));
        assert!(!wants_read_stub(Some("2.1.276 (Claude Code)")));
        assert!(wants_read_stub(Some("2.1.317")));
        assert!(!wants_read_stub(None));
    }
}
