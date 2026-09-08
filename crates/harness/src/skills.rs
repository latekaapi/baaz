//! Skills for the `/` menu.
//!
//! Skills reach MSP only as ordinary `toolCall` items — there is no skills
//! method on the wire — so the list comes from the CLI: `muse skills list
//! --json`, run once at boot on a background thread. Its shape is
//!
//! ```json
//! {"skills":[{"id":"bundled:browser-app-delivery","name":"browser-app-delivery",
//!             "display_name":"…","description":"…"}]}
//! ```
//!
//! and the scope is the prefix of `id` (`bundled` / `user` / `project` /
//! `plugin`), which is exactly what `CommandItem::source_tag` shows. A skill row
//! inserts `/name ` as text; the server resolves it, as the Phase 3 `/plan`
//! probe proved.

use std::process::Command;

use serde::Deserialize;

/// One row of `muse skills list --json`.
#[derive(Clone, Debug, Deserialize)]
pub struct Skill {
    /// `bundled:plan`, `user:my-thing`.
    pub id: String,
    /// The name the slash command uses.
    pub name: String,
    /// The human label; often the same as the name.
    #[serde(default)]
    pub display_name: Option<String>,
    /// One-line description.
    #[serde(default)]
    pub description: Option<String>,
}

impl Skill {
    /// The scope tag, from the `id` prefix. An id with no prefix is `skill`,
    /// because a tag that guessed would be worse than one that does not.
    pub fn scope(&self) -> &str {
        match self.id.split_once(':') {
            Some((scope, _)) => scope,
            None => "skill",
        }
    }

    /// The description, trimmed to one readable line.
    pub fn summary(&self) -> String {
        let text = self.description.clone().unwrap_or_default();
        let line = text.split(['\n', '.']).find(|s| !s.trim().is_empty()).unwrap_or("").trim();
        if line.is_empty() {
            self.display_name.clone().unwrap_or_else(|| self.name.clone())
        } else {
            format!("{line}.")
        }
    }
}

#[derive(Deserialize)]
struct Listing {
    #[serde(default)]
    skills: Vec<Skill>,
}

/// Run `muse skills list --json`.
///
/// Blocking, and forgiving: a `muse` that is not there, a non-zero exit and a
/// shape this build has never seen all yield an empty list, because the `/` menu
/// still works without a Skills section.
pub fn list(program: &str) -> Vec<Skill> {
    let Ok(output) = Command::new(program).args(["skills", "list", "--json"]).output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    serde_json::from_slice::<Listing>(&output.stdout).map(|l| l.skills).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(id: &str, description: Option<&str>) -> Skill {
        Skill {
            id: id.to_owned(),
            name: id.split_once(':').map(|(_, n)| n).unwrap_or(id).to_owned(),
            display_name: None,
            description: description.map(str::to_owned),
        }
    }

    #[test]
    fn the_scope_is_the_id_prefix() {
        assert_eq!(skill("bundled:plan", None).scope(), "bundled");
        assert_eq!(skill("user:mine", None).scope(), "user");
        assert_eq!(skill("plan", None).scope(), "skill");
    }

    #[test]
    fn the_summary_is_one_sentence() {
        let s = skill("bundled:plan", Some("Create a grounded plan. Then stop.\nMore prose."));
        assert_eq!(s.summary(), "Create a grounded plan.");
    }

    #[test]
    fn a_skill_with_no_description_falls_back_to_its_name() {
        assert_eq!(skill("bundled:plan", None).summary(), "plan");
    }
}
