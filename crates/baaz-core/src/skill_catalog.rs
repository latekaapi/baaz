//! One catalog of skills across providers (docs/85 16.2, W8): every
//! skill folder Baaz knows, read from disk, each skill with the providers
//! that see it — so one page can list them all and the `/` menu can offer
//! them on any provider. Muse's own list stays its CLI's
//! ([`crate::skills`]); this covers the folders.
//!
//! Who sees what (probed 25 Nov 2026 where it says so):
//! * `~/.agents/skills` — Codex natively (probed, codex 0.144.6), Claude
//!   Code through Baaz's per-skill links in its home.
//! * a repo's `.agents/skills` — Codex natively (probed). Claude Code does
//!   not read it (probed 26 Nov 2026, claude 2.1.276); it sees it through
//!   Baaz's per-repo folder of links handed over with `--add-dir`.
//! * `~/.claude/skills`, a repo's `.claude/skills` — Claude Code.
//! * `~/.codex/skills`, a repo's `.codex/skills` — Codex.
//!
//! A provider that loads no skill folder (Muse for these folders, ACP) is
//! offered a skill as its SKILL.md pasted into the prompt, capped
//! ([`paste_text`]).

use std::path::{Path, PathBuf};

/// Where a skill lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// `~/.agents/skills`.
    Agents,
    /// A repo's `.agents/skills`.
    RepoAgents,
    /// `~/.claude/skills`.
    Claude,
    /// A repo's `.claude/skills`.
    RepoClaude,
    /// `~/.codex/skills`.
    Codex,
    /// A repo's `.codex/skills`.
    RepoCodex,
}

impl Source {
    /// The providers (wire ids) that load skills from here themselves.
    pub fn seen_by(self) -> &'static [&'static str] {
        match self {
            Self::Agents | Self::RepoAgents => &["claude-code", "codex"],
            Self::Codex | Self::RepoCodex => &["codex"],
            Self::Claude | Self::RepoClaude => &["claude-code"],
        }
    }

    /// The folder's own words for the page (`~/.agents/skills`).
    pub fn label(self) -> &'static str {
        match self {
            Self::Agents => "~/.agents/skills",
            Self::RepoAgents => ".agents/skills",
            Self::Claude => "~/.claude/skills",
            Self::RepoClaude => ".claude/skills",
            Self::Codex => "~/.codex/skills",
            Self::RepoCodex => ".codex/skills",
        }
    }

    /// Whether the folder belongs to the project rather than the person.
    pub fn in_repo(self) -> bool {
        matches!(self, Self::RepoAgents | Self::RepoClaude | Self::RepoCodex)
    }
}

/// One skill in the catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogSkill {
    /// Its name (the frontmatter's, else its folder's).
    pub name: String,
    /// Its one-line description, when the frontmatter has one.
    pub description: Option<String>,
    /// Its SKILL.md.
    pub path: PathBuf,
    /// Where it lives.
    pub source: Source,
}

impl CatalogSkill {
    /// The providers that load it themselves.
    pub fn seen_by(&self) -> &'static [&'static str] {
        self.source.seen_by()
    }
}

/// A SKILL.md's `name` and `description` from its frontmatter.
pub fn frontmatter(text: &str) -> (Option<String>, Option<String>) {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, None);
    }
    let (mut name, mut description) = (None, None);
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        let value = |key: &str| line.strip_prefix(key).map(|v| v.trim().trim_matches('"').trim_matches('\'').to_owned()).filter(|v| !v.is_empty());
        name = name.or_else(|| value("name:"));
        description = description.or_else(|| value("description:"));
    }
    (name, description)
}

fn scan_dir(dir: &Path, source: Source, out: &mut Vec<CatalogSkill>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut found: Vec<CatalogSkill> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            // A per-skill link (Baaz's Claude home) or a real folder.
            let path = e.path().join("SKILL.md");
            let text = std::fs::read_to_string(&path).ok()?;
            let (name, description) = frontmatter(&text);
            Some(CatalogSkill { name: name.unwrap_or_else(|| e.file_name().to_string_lossy().into_owned()), description, path, source })
        })
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    out.extend(found);
}

/// Every skill in the folders above, for the person at `home` and the
/// project at `repo`: the project's first, then the person's, each folder
/// in name order.
pub fn scan(home: &Path, repo: Option<&Path>) -> Vec<CatalogSkill> {
    let mut out = Vec::new();
    if let Some(repo) = repo {
        scan_dir(&repo.join(".agents/skills"), Source::RepoAgents, &mut out);
        scan_dir(&repo.join(".claude/skills"), Source::RepoClaude, &mut out);
        scan_dir(&repo.join(".codex/skills"), Source::RepoCodex, &mut out);
    }
    scan_dir(&home.join(".agents/skills"), Source::Agents, &mut out);
    scan_dir(&home.join(".claude/skills"), Source::Claude, &mut out);
    scan_dir(&home.join(".codex/skills"), Source::Codex, &mut out);
    out
}

/// How much of a SKILL.md a pasted skill carries.
pub const PASTE_CAP: usize = 8_000;

/// A skill pasted into the prompt for a provider that loads none: its body
/// (frontmatter off), capped at [`PASTE_CAP`] characters.
pub fn paste_text(skill: &CatalogSkill) -> Option<String> {
    let text = std::fs::read_to_string(&skill.path).ok()?;
    let body = crate::skills::strip_frontmatter(&text).trim();
    let capped: String = body.chars().take(PASTE_CAP).collect();
    let cut = if body.chars().count() > PASTE_CAP { "\n\n[…the rest of the skill is cut here]" } else { "" };
    Some(format!("Use the skill \"{}\":\n\n{capped}{cut}\n\n", skill.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(dir: &Path, folder: &str, text: &str) {
        std::fs::create_dir_all(dir.join(folder)).unwrap();
        std::fs::write(dir.join(folder).join("SKILL.md"), text).unwrap();
    }

    #[test]
    fn the_catalog_reads_every_folder_and_says_who_sees_each_skill() {
        let root = std::env::temp_dir().join(format!("skill-catalog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (home, repo) = (root.join("home"), root.join("repo"));
        skill(&home.join(".agents/skills"), "shared", "---\nname: shared\ndescription: \"Shared by all\"\n---\n\nDo the shared thing.");
        skill(&home.join(".claude/skills"), "review", "---\nname: review\n---\nReview.");
        skill(&home.join(".codex/skills"), "lint", "no frontmatter");
        skill(&repo.join(".agents/skills"), "deploy", "---\ndescription: Ship it\n---\nShip.");
        std::fs::create_dir_all(home.join(".agents/skills/not-a-skill")).unwrap();
        let all = scan(&home, Some(&repo));
        let rows: Vec<(&str, Source, &[&str])> = all.iter().map(|s| (s.name.as_str(), s.source, s.seen_by())).collect();
        assert_eq!(rows, vec![
            ("deploy", Source::RepoAgents, &["claude-code", "codex"][..]),
            ("shared", Source::Agents, &["claude-code", "codex"][..]),
            ("review", Source::Claude, &["claude-code"][..]),
            ("lint", Source::Codex, &["codex"][..]),
        ]);
        assert_eq!(all[1].description.as_deref(), Some("Shared by all"));
        assert_eq!(all[0].description.as_deref(), Some("Ship it"), "a name from its folder, a description from the file");
        let pasted = paste_text(&all[1]).unwrap();
        assert!(pasted.starts_with("Use the skill \"shared\":") && pasted.contains("Do the shared thing.") && !pasted.contains("description:"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
