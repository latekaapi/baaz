//! The skill folders every provider shares, the app's half (docs/85 16.2):
//! the catalog read from disk (cached briefly — the `/` menu asks on every
//! keystroke), each provider's own switches, and what the `/` menu offers
//! on a session: the folder skills its provider loads (with their
//! descriptions; a switched-off one hidden), and on a provider that loads
//! no folder, the same skills as text to paste.
//!
//! Not keyed by session id: per workspace and provider.

use baaz_core::skill_catalog::{CatalogSkill, Source};
use std::sync::atomic::{AtomicBool, Ordering};

/// The `/` menu row's scope for a pasted skill.
pub(crate) const PASTE_SCOPE: &str = "paste";

/// Set by the capture fixture: captures read these instead of the
/// machine's folders (which would differ run to run).
static FIXTURE: AtomicBool = AtomicBool::new(false);

const SOURCES: [Source; 6] = [Source::Agents, Source::RepoAgents, Source::Claude, Source::RepoClaude, Source::Codex, Source::RepoCodex];

/// Whether `wire` loads any skill folder itself.
pub(crate) fn loads_folders(wire: &str) -> bool {
    SOURCES.iter().any(|s| s.seen_by().contains(&wire))
}

/// The key `wire`'s own switch names a skill by: Codex's SKILL.md path,
/// Claude Code's skill name.
pub(crate) fn switch_key(wire: &str, skill: &CatalogSkill) -> String {
    if wire == "codex" { skill.path.to_string_lossy().into_owned() } else { skill.name.clone() }
}

/// The folders' skills for a session in `repo`: the machine's, cached two
/// seconds; none in a capture unless its fixture is on.
pub(crate) fn catalog(repo: &str) -> Vec<CatalogSkill> {
    use std::sync::Mutex;
    type Kept = (std::time::Instant, String, Vec<CatalogSkill>);
    static KEPT: Mutex<Option<Kept>> = Mutex::new(None);
    // A capture or a test never reads the machine's folders.
    if (crate::runtime::captures() || cfg!(test)) && !FIXTURE.load(Ordering::Relaxed) {
        return Vec::new();
    }
    let mut kept = KEPT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((at, for_repo, skills)) = kept.as_ref() {
        if for_repo == repo && at.elapsed() < std::time::Duration::from_secs(2) {
            return skills.clone();
        }
    }
    let skills = if FIXTURE.load(Ordering::Relaxed) { fixture() } else {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
        baaz_core::skill_catalog::scan(&home, (!repo.is_empty()).then(|| std::path::Path::new(repo)))
    };
    *kept = Some((std::time::Instant::now(), repo.to_owned(), skills.clone()));
    skills
}

/// The `/` menu's skills on a `wire` session: `base` (the provider's own
/// list, or the managed catalog) with the folder skills it loads added or
/// described, switched-off ones hidden; on a provider that loads no
/// folder, the folder skills as paste rows it does not already have.
pub(crate) fn menu_rows(wire: &str, repo: &str, base: Vec<crate::skills::Skill>) -> Vec<crate::skills::Skill> {
    let folders = catalog(repo);
    if folders.is_empty() {
        return base;
    }
    let switches = provider::skill_switches::load(&provider::child_env::state_dir());
    merge(wire, folders, &switches, base)
}

/// [`menu_rows`]' merge, given the folders and the switches.
fn merge(wire: &str, folders: Vec<CatalogSkill>, switches: &provider::skill_switches::SkillSwitches, mut base: Vec<crate::skills::Skill>) -> Vec<crate::skills::Skill> {
    let native = loads_folders(wire);
    for skill in folders {
        if skill.seen_by().contains(&wire) {
            if !switches.is_on(wire, &switch_key(wire, &skill)) {
                base.retain(|s| s.name != skill.name);
                continue;
            }
            match base.iter_mut().find(|s| s.name == skill.name) {
                Some(row) if row.description.is_none() => row.description = skill.description.clone(),
                Some(_) => {}
                None => base.push(crate::skills::Skill { id: skill.name.clone(), name: skill.name.clone(), description: skill.description.clone(), ..Default::default() }),
            }
        } else if !native && !base.iter().any(|s| s.name == skill.name) {
            base.push(crate::skills::Skill {
                id: format!("paste:{}", skill.path.display()),
                name: skill.name.clone(),
                description: skill.description.clone(),
                scope: Some(PASTE_SCOPE.to_owned()),
                path: Some(skill.path.to_string_lossy().into_owned()),
                ..Default::default()
            });
        }
    }
    base
}

/// What a chosen paste row puts in the composer: the skill's body, capped.
pub(crate) fn paste_for(skill: &crate::skills::Skill) -> Option<String> {
    let path = skill.path.as_deref()?;
    let catalog_skill = CatalogSkill { name: skill.name.clone(), description: skill.description.clone(), path: path.into(), source: Source::Agents };
    baaz_core::skill_catalog::paste_text(&catalog_skill)
}

/// The capture fixture's skills (docs/85 16.2): three folders, no disk.
fn fixture() -> Vec<CatalogSkill> {
    let skill = |name: &str, desc: &str, path: &str, source| CatalogSkill { name: name.into(), description: Some(desc.into()), path: path.into(), source };
    vec![
        skill("deploy", "Ship the web app to staging", "/fixture/acme-web/.agents/skills/deploy/SKILL.md", Source::RepoAgents),
        skill("review-pr", "Review a pull request against the team checklist", "/fixture/home/.agents/skills/review-pr/SKILL.md", Source::Agents),
        skill("release-notes", "Draft release notes from the merged PRs", "/fixture/home/.claude/skills/release-notes/SKILL.md", Source::Claude),
    ]
}

/// `skills-folders-fixture` (captures): the Skills page's folder section
/// and the `/` menu read the fixture skills.
#[cfg(feature = "probe")]
pub(crate) fn step_fixture() -> crate::steps::StepResult {
    if !crate::runtime::captures() {
        return Err("skills-folders-fixture: captures only".into());
    }
    FIXTURE.store(true, Ordering::Relaxed);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_adds_described_folder_skills_hides_switched_off_and_pastes_elsewhere() {
        let folders = fixture();
        let mut switches = provider::skill_switches::SkillSwitches::default();
        switches.off.entry("codex".into()).or_default().insert("/fixture/acme-web/.agents/skills/deploy/SKILL.md".into());
        // Claude Code listed review-pr by name only: the folder describes it.
        let base = vec![crate::skills::Skill { id: "review-pr".into(), name: "review-pr".into(), ..Default::default() }];
        let claude = merge("claude-code", folders.clone(), &switches, base);
        let names: Vec<(&str, Option<&str>)> = claude.iter().map(|s| (s.name.as_str(), s.description.as_deref())).collect();
        assert_eq!(names, vec![("review-pr", Some("Review a pull request against the team checklist")), ("release-notes", Some("Draft release notes from the merged PRs"))]);
        let codex: Vec<String> = merge("codex", folders.clone(), &switches, Vec::new()).into_iter().map(|s| s.name).collect();
        assert_eq!(codex, vec!["review-pr"], "deploy is switched off for Codex");
        let muse = merge("muse", folders, &switches, Vec::new());
        assert_eq!(muse.len(), 3);
        assert!(muse.iter().all(|s| s.scope.as_deref() == Some(PASTE_SCOPE) && s.path.is_some()), "Muse loads no folder: every one pastes");
    }

    #[test]
    fn who_loads_folders_and_how_switches_name_a_skill() {
        assert!(loads_folders("codex") && loads_folders("claude-code"));
        assert!(!loads_folders("muse") && !loads_folders("acp"));
        let s = CatalogSkill { name: "deploy".into(), description: None, path: "/r/.agents/skills/deploy/SKILL.md".into(), source: Source::RepoAgents };
        assert_eq!(switch_key("codex", &s), "/r/.agents/skills/deploy/SKILL.md");
        assert_eq!(switch_key("claude-code", &s), "deploy");
    }
}
