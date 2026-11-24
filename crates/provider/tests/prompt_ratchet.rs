//! docs/85 10.5, rule 17: nothing but the note's own two channels writes a
//! provider's system prompt. Scans every crate's source for the flags and
//! fields that set one; only `provider-claude-code/src/argv.rs`
//! (`with_appended_note`) and `provider-codex/src/child.rs` (`with_note`)
//! may name them in code. Comments and a file's trailing test module are
//! skipped: they are not what ships.

use std::path::{Path, PathBuf};

const PATTERNS: [&str; 6] = [
    "--append-system-prompt",
    "--system-prompt",
    "developerInstructions",
    "baseInstructions",
    "systemPrompt",
    "appendSystemPrompt",
];

const ALLOWED: [&str; 2] = ["provider-claude-code/src/argv.rs", "provider-codex/src/child.rs"];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// The code before any `//` on a line (a string holding `//` ends the
/// line early, which can only hide a match, never invent one).
fn code_of(line: &str) -> &str {
    line.split("//").next().unwrap_or("")
}

fn offenders(crates: &Path) -> Vec<String> {
    let mut files = Vec::new();
    rust_files(crates, &mut files);
    let mut found = Vec::new();
    for file in files {
        let rel = file.strip_prefix(crates).unwrap_or(&file).to_string_lossy().replace('\\', "/");
        if ALLOWED.contains(&rel.as_str()) || rel.ends_with("tests/prompt_ratchet.rs") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        let lines: Vec<&str> = text.lines().collect();
        for (n, line) in lines.iter().enumerate() {
            // A file's test module (`#[cfg(test)]` then `mod …`) asserts on
            // these names; the ratchet is about what ships.
            if line.trim() == "#[cfg(test)]" && lines.get(n + 1).is_some_and(|next| next.trim_start().starts_with("mod ")) {
                break;
            }
            let code = code_of(line);
            for pattern in PATTERNS {
                if code.contains(pattern) {
                    found.push(format!("{rel}:{} names {pattern}", n + 1));
                }
            }
        }
    }
    found
}

#[test]
fn only_the_notes_channels_write_a_system_prompt() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let found = offenders(&crates);
    assert!(found.is_empty(), "a system-prompt flag outside the note's channels (rule 17):\n{}", found.join("\n"));
}

#[test]
fn the_scan_catches_a_planted_flag() {
    let dir = std::env::temp_dir().join(format!("prompt-ratchet-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("x/src")).expect("dir");
    std::fs::write(
        dir.join("x/src/lib.rs"),
        "fn f() { let a = [\"--system-prompt\", \"x\"]; } // --append-system-prompt in a comment\n\
         #[cfg(test)]\nmod tests { const A: &str = \"developerInstructions\"; }\n",
    )
    .expect("write");
    let found = offenders(&dir);
    assert_eq!(found.len(), 1, "{found:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
