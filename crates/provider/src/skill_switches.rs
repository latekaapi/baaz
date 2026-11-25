//! Which skills the person switched off, per provider (docs/85 16.2): one
//! small file in Baaz's state dir, `skills-off.tsv`, a line per switch —
//! `<provider wire id>\t<key>`. The key is what the provider's own switch
//! names: Claude Code's skill name (its `skillOverrides`), Codex's
//! SKILL.md path (its `skills.config`). Each adapter reads it at spawn and
//! passes it on its own argv (rule 26: never through a linked owner file).
//! Nothing here writes into a provider's home.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The file's name under the state dir.
pub const FILE: &str = "skills-off.tsv";

/// The switched-off keys, per provider.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SkillSwitches {
    /// Provider wire id → its switched-off keys.
    pub off: BTreeMap<String, BTreeSet<String>>,
}

fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE)
}

impl SkillSwitches {
    /// The keys `provider` has switched off.
    pub fn off_for(&self, provider: &str) -> Vec<String> {
        self.off.get(provider).map(|keys| keys.iter().cloned().collect()).unwrap_or_default()
    }

    /// Whether `key` is on for `provider` (everything is, until switched off).
    pub fn is_on(&self, provider: &str, key: &str) -> bool {
        !self.off.get(provider).is_some_and(|keys| keys.contains(key))
    }
}

/// The switches in `state_dir` (none when the file is missing).
pub fn load(state_dir: &Path) -> SkillSwitches {
    let mut out = SkillSwitches::default();
    let Ok(text) = std::fs::read_to_string(path(state_dir)) else { return out };
    for line in text.lines() {
        if let Some((provider, key)) = line.split_once('\t') {
            if !provider.is_empty() && !key.is_empty() {
                out.off.entry(provider.to_owned()).or_default().insert(key.to_owned());
            }
        }
    }
    out
}

/// Switch `key` on or off for `provider`, rewriting the file whole.
pub fn set(state_dir: &Path, provider: &str, key: &str, on: bool) -> std::io::Result<SkillSwitches> {
    let mut switches = load(state_dir);
    let keys = switches.off.entry(provider.to_owned()).or_default();
    if on {
        keys.remove(key);
    } else {
        keys.insert(key.to_owned());
    }
    switches.off.retain(|_, keys| !keys.is_empty());
    let text: String = switches.off.iter().flat_map(|(p, keys)| keys.iter().map(move |k| format!("{p}\t{k}\n"))).collect();
    std::fs::create_dir_all(state_dir)?;
    std::fs::write(path(state_dir), text)?;
    Ok(switches)
}

/// Codex's argv override for its switched-off skills (probed 25 Nov 2026,
/// codex 0.144.6: `-c skills.config=[{path=…,enabled=false}]` turns the
/// skill off in `skills/list`): `None` with none off.
pub fn codex_config_arg(off: &[String]) -> Option<String> {
    if off.is_empty() {
        return None;
    }
    let entries: Vec<String> = off.iter().map(|path| format!("{{path={},enabled=false}}", toml_string(path))).collect();
    Some(format!("skills.config=[{}]", entries.join(",")))
}

fn toml_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches_round_trip_and_become_codex_argv() {
        let dir = std::env::temp_dir().join(format!("skill-switches-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(load(&dir).is_on("codex", "/a/SKILL.md"));
        set(&dir, "codex", "/a/SKILL.md", false).unwrap();
        set(&dir, "claude-code", "review", false).unwrap();
        let s = load(&dir);
        assert!(!s.is_on("codex", "/a/SKILL.md") && !s.is_on("claude-code", "review") && s.is_on("claude-code", "other"));
        assert_eq!(codex_config_arg(&s.off_for("codex")).unwrap(), "skills.config=[{path=\"/a/SKILL.md\",enabled=false}]");
        set(&dir, "codex", "/a/SKILL.md", true).unwrap();
        assert!(load(&dir).off_for("codex").is_empty());
        assert_eq!(codex_config_arg(&[]), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
